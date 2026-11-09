//! Local automatic-recording policy with a separate, trusted consent store.
//!
//! A repository document is configuration, never authorization. Automatic modes
//! become effective only while its exact document and directory identity match
//! a native-user-confirmed grant in the Rig home. This module owns no process,
//! network access, UI, retry policy, or Git metadata.

use std::collections::BTreeMap;
use std::fs::{File, Metadata};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rig_harness::prelude::AgentId;
use rustix::fs::{AtFlags, CWD, Mode as FileMode, OFlags, mkdirat, openat, renameat, unlinkat};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

const VERSION: u32 = 1;
const MAX_BYTES: u64 = 1024 * 1024;
const MAX_GRANTS: usize = 256;
const MAX_AUDIT_BYTES: usize = 16 * 1024;

/// Setup modes shared by the Delta CLI integrations.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Mode {
    #[default]
    Manual,
    Private,
    Organization,
}

/// Only trusted native setup may select export options. The shared repository
/// document names mode and organization; its unknown export fields cannot opt
/// an agent into reasoning or binary-media disclosure.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Policy {
    pub mode: Mode,
    pub organization: Option<String>,
    pub reasoning: bool,
    pub media: bool,
}

impl Policy {
    pub(crate) fn records_automatically(&self) -> bool {
        matches!(self.mode, Mode::Private | Mode::Organization)
    }

    fn validate(&self) -> Result<(), String> {
        if let Some(organization) = &self.organization
            && (organization.is_empty()
                || organization.len() > 4096
                || organization.starts_with('-')
                || organization
                    .chars()
                    .any(|character| character.is_whitespace() || character.is_control()))
        {
            return Err(
                "Delta policy organization must be a nonblank, bounded organization ID".into(),
            );
        }
        if self.records_automatically() && self.organization.is_none() {
            return Err("Automatic Delta policy requires an explicit organization ID".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PolicyFile {
    pub policy: Policy,
    pub configured: bool,
    pub problem: Option<String>,
    /// Preserved for setup rewrites; never treated as proof of consent.
    pub document: Option<Map<String, Value>>,
}

impl PolicyFile {
    fn absent() -> Self {
        Self {
            policy: Policy::default(),
            configured: false,
            problem: None,
            document: None,
        }
    }

    fn refused(message: impl Into<String>) -> Self {
        Self {
            problem: Some(message.into()),
            ..Self::absent()
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct EffectivePolicy {
    pub policy: Policy,
    /// This reports a matching local consent record, not execution permission.
    pub trusted: bool,
    pub problem: Option<String>,
    /// The native runtime must independently verify the recorded process scope,
    /// executable/config identity and Person provenance before automatic starts.
    pub consent_audit: Option<String>,
}

impl EffectivePolicy {
    fn manual(problem: impl Into<String>) -> Self {
        Self {
            policy: Policy::default(),
            trusted: false,
            problem: Some(problem.into()),
            consent_audit: None,
        }
    }
}

/// An immutable setup plan for the native confirmation dialog. Preparing it has
/// no filesystem writes. Saving refuses if either directory or source document
/// changed while the user was deciding.
#[derive(Clone, Debug)]
pub(crate) struct PreparedPolicy {
    root: PathBuf,
    identity: DirectoryIdentity,
    // The native question may outlive a pathname replacement. Shared immutable
    // ownership keeps the reviewed directory itself pinned across plan clones.
    root_fd: Arc<File>,
    previous: Option<Map<String, Value>>,
    document: Map<String, Value>,
    policy: Policy,
}

impl PreparedPolicy {
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
    pub(crate) fn policy(&self) -> &Policy {
        &self.policy
    }
    pub(crate) fn document(&self) -> &Map<String, Value> {
        &self.document
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Grant {
    identity: DirectoryIdentity,
    policy: Policy,
    document: Map<String, Value>,
    consent_audit: String,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Store {
    version: u32,
    grants: BTreeMap<String, Grant>,
}

impl Store {
    fn empty() -> Self {
        Self {
            version: VERSION,
            grants: BTreeMap::new(),
        }
    }
}

/// Read only the shared project document. Missing, malformed and semantically
/// invalid policy remains manual; diagnostics do not include document contents.
pub(crate) fn read(root: &Path) -> PolicyFile {
    let root = match pin_checkout(root) {
        Ok(root) => root,
        Err(error) => return PolicyFile::refused(error),
    };
    match project_document(&root.descriptor) {
        Ok(Some(document)) => parse_document(document),
        Ok(None) => PolicyFile::absent(),
        Err(error) => PolicyFile::refused(error),
    }
}

fn parse_document(document: Map<String, Value>) -> PolicyFile {
    let mut result = PolicyFile {
        document: Some(document.clone()),
        ..PolicyFile::absent()
    };
    let Some(threads) = document.get("threads") else {
        return result;
    };
    let Some(threads) = threads.as_object() else {
        result.problem = Some("Delta policy threads must be an object".into());
        return result;
    };
    let Some(mode) = threads.get("mode") else {
        return result;
    };
    let mode = match mode.as_str() {
        Some("manual") => Mode::Manual,
        Some("private") => Mode::Private,
        Some("organization") => Mode::Organization,
        _ => {
            result.problem =
                Some("Delta policy mode must be manual, private or organization".into());
            return result;
        }
    };
    let organization = match threads.get("organization") {
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => {
            result.problem = Some("Delta policy organization must be a string".into());
            return result;
        }
        None => None,
    };
    let policy = Policy {
        mode,
        organization,
        ..Policy::default()
    };
    if let Err(error) = policy.validate() {
        result.problem = Some(error);
        return result;
    }
    result.policy = policy;
    result.configured = true;
    result
}

/// Freeze a selected policy and the current source document for native consent.
/// Known fields are replaced; unknown top-level and nested keys are preserved.
pub(crate) fn prepare(root: &Path, policy: &Policy) -> Result<PreparedPolicy, String> {
    policy.validate()?;
    let root = pin_checkout(root)?;
    let previous = project_document(&root.descriptor)?;
    let mut document = previous.clone().unwrap_or_default();
    let mut threads = match document.remove("threads") {
        Some(Value::Object(threads)) => threads,
        Some(_) => return Err(
            "Cannot replace non-object Delta threads policy; inspect the existing settings first"
                .into(),
        ),
        None => Map::new(),
    };
    let mode = match policy.mode {
        Mode::Manual => "manual",
        Mode::Private => "private",
        Mode::Organization => "organization",
    };
    threads.insert("mode".into(), Value::String(mode.into()));
    match &policy.organization {
        Some(organization) => {
            threads.insert("organization".into(), Value::String(organization.clone()));
        }
        None => {
            threads.remove("organization");
        }
    }
    document.insert("threads".into(), Value::Object(threads));
    bounded_value(&document)?;
    Ok(PreparedPolicy {
        root: root.path,
        identity: root.identity,
        root_fd: root.descriptor,
        previous,
        document,
        policy: policy.clone(),
    })
}

/// Resolve an automatic mode only when a trusted local grant matches the exact
/// project document, selected mode/organization and canonical directory identity.
/// This does not authorize starting a process or publishing to another audience.
pub(crate) fn effective(home: &Path, root: &Path) -> EffectivePolicy {
    let pinned = match pin_checkout(root) {
        Ok(root) => root,
        Err(error) => return EffectivePolicy::manual(error),
    };
    let root = &pinned.path;
    let file = match project_document(&pinned.descriptor) {
        Ok(Some(document)) => parse_document(document),
        Ok(None) => PolicyFile::absent(),
        Err(error) => PolicyFile::refused(error),
    };
    if let Some(problem) = file.problem {
        return EffectivePolicy::manual(problem);
    }
    if !file.policy.records_automatically() {
        return EffectivePolicy {
            policy: file.policy,
            trusted: false,
            problem: None,
            consent_audit: None,
        };
    }
    if let Err(error) = trusted_store_is_separate(home, root) {
        return EffectivePolicy::manual(error);
    }
    let directory = match state_directory(home, false) {
        Ok(Some(directory)) => directory,
        Ok(None) => {
            return EffectivePolicy::manual(
                "Project policy has no trusted local consent; run /delta setup before automatic recording",
            );
        }
        Err(error) => return EffectivePolicy::manual(error),
    };
    let store = match read_store(&directory) {
        Ok(store) => store,
        Err(error) => return EffectivePolicy::manual(error),
    };
    let key = match root.to_str() {
        Some(key) => key,
        None => {
            return EffectivePolicy::manual(
                "Delta policy requires a UTF-8 canonical checkout path",
            );
        }
    };
    let Some(grant) = store.grants.get(key) else {
        return EffectivePolicy::manual(
            "Project policy has no trusted local consent; run /delta setup before automatic recording",
        );
    };
    if !matches_named_directory(root, &pinned.identity) {
        return EffectivePolicy::manual(
            "Delta checkout directory changed while reading its policy",
        );
    }
    if grant.identity != pinned.identity
        || grant.policy.mode != file.policy.mode
        || grant.policy.organization != file.policy.organization
        || file.document.as_ref() != Some(&grant.document)
    {
        return EffectivePolicy::manual(
            "Delta policy or checkout identity changed since consent; run /delta setup to confirm the new scope",
        );
    }
    EffectivePolicy {
        policy: grant.policy.clone(),
        trusted: true,
        problem: None,
        consent_audit: Some(grant.consent_audit.clone()),
    }
}

/// Commit a plan only after the caller has verified an actual native user's
/// Person decision for this exact plan. `consent_audit` is durable provenance,
/// not a boolean supplied by a model tool or repository file. It does not grant
/// host execution authority; the process capability still needs its own consent.
///
/// Existing consent is durably removed before the project document is changed.
/// A later failure therefore disables automation instead of retaining a stale
/// or accidentally broader grant. No Git files or worktree contents are removed.
pub(crate) fn save_confirmed(
    home: &Path,
    plan: &PreparedPolicy,
    consent_audit: &str,
) -> Result<(), String> {
    if consent_audit.trim().is_empty() || consent_audit.len() > MAX_AUDIT_BYTES {
        return Err(
            "Confirmed Delta setup requires nonblank native-consent provenance of at most 16 KiB"
                .into(),
        );
    }
    plan.policy.validate()?;
    if plan.policy.records_automatically() {
        trusted_store_is_separate(home, &plan.root)?;
    }
    validate_plan(plan)?;
    let directory =
        state_directory(home, true)?.ok_or("Cannot create trusted Delta policy directory")?;
    let _lock = lock(&directory)?;
    validate_plan(plan)?;
    let mut store = read_store(&directory)?;
    let key = plan
        .root
        .to_str()
        .ok_or("Delta policy requires a UTF-8 canonical checkout path")?
        .to_owned();
    store.grants.remove(&key);
    write_json_at(&directory, "policies.json", &store)?;

    let project = project_directory(&plan.root_fd, true)?
        .ok_or("Cannot create Delta project settings directory")?;
    if document_in(&project)? != plan.previous {
        return Err(
            "Delta settings changed during setup; automatic recording remains disabled".into(),
        );
    }
    write_json_at(&project, "settings.json", &plan.document)?;
    if !matches_named_directory(&plan.root, &plan.identity)
        || project_document(&plan.root_fd)?.as_ref() != Some(&plan.document)
    {
        return Err(
            "Delta checkout or settings changed during setup; automatic recording remains disabled"
                .into(),
        );
    }
    if plan.policy.records_automatically() {
        if store.grants.len() >= MAX_GRANTS {
            return Err("Trusted Delta policies exceed the 256-checkout limit; automatic recording remains disabled".into());
        }
        store.grants.insert(
            key,
            Grant {
                identity: plan.identity.clone(),
                policy: plan.policy.clone(),
                document: plan.document.clone(),
                consent_audit: consent_audit.into(),
            },
        );
        write_json_at(&directory, "policies.json", &store)?;
    }
    Ok(())
}

/// Remove only local automatic-recording consent. The project document stays as
/// it is, but can no longer activate recording until native setup confirms it.
pub(crate) fn revoke(home: &Path, root: &Path) -> Result<(), String> {
    let root = canonical_directory(root)?;
    let Some(directory) = state_directory(home, false)? else {
        return Ok(());
    };
    let _lock = lock(&directory)?;
    let mut store = read_store(&directory)?;
    let key = root
        .to_str()
        .ok_or("Delta policy requires a UTF-8 canonical checkout path")?;
    if store.grants.remove(key).is_some() {
        write_json_at(&directory, "policies.json", &store)?;
    }
    Ok(())
}

fn validate_plan(plan: &PreparedPolicy) -> Result<(), String> {
    if !matches_named_directory(&plan.root, &plan.identity)
        || identity_of(
            &plan
                .root_fd
                .metadata()
                .map_err(|error| format!("Cannot inspect pinned Delta checkout: {error}"))?,
        )? != plan.identity
        || project_document(&plan.root_fd)? != plan.previous
    {
        return Err("Delta checkout or settings changed while confirming setup; prepare and confirm the new scope".into());
    }
    Ok(())
}

fn canonical_directory(path: &Path) -> Result<PathBuf, String> {
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("Cannot resolve Delta checkout directory: {error}"))?;
    if !canonical.is_dir() {
        return Err("Delta checkout path must be a directory".into());
    }
    if canonical.parent().is_none() {
        return Err("The filesystem root cannot be an automatic Delta checkout".into());
    }
    Ok(canonical)
}

#[derive(Clone, Debug)]
struct PinnedCheckout {
    path: PathBuf,
    descriptor: Arc<File>,
    identity: DirectoryIdentity,
}

fn pin_checkout(path: &Path) -> Result<PinnedCheckout, String> {
    let path = canonical_directory(path)?;
    let descriptor = Arc::new(open_directory(&path)?);
    let identity = identity_of(
        &descriptor
            .metadata()
            .map_err(|error| format!("Cannot inspect pinned Delta checkout: {error}"))?,
    )?;
    if !matches_named_directory(&path, &identity) {
        return Err("Delta checkout changed while preparing its directory descriptor".into());
    }
    Ok(PinnedCheckout {
        path,
        descriptor,
        identity,
    })
}

fn open_directory(path: &Path) -> Result<File, String> {
    openat(CWD, path, directory_flags(), FileMode::empty())
        .map(File::from)
        .map_err(|error| format!("Cannot open non-symlink Delta directory: {error}"))
}

fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK
}

fn identity_of(metadata: &Metadata) -> Result<DirectoryIdentity, String> {
    if !metadata.is_dir() {
        return Err("Pinned Delta checkout is not a directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(DirectoryIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(not(unix))]
    {
        Err("Trusted Delta policy requires supported filesystem identity checks".into())
    }
}

fn matches_named_directory(path: &Path, expected: &DirectoryIdentity) -> bool {
    open_directory(path)
        .ok()
        .and_then(|directory| directory.metadata().ok())
        .and_then(|metadata| identity_of(&metadata).ok())
        .as_ref()
        == Some(expected)
}

/// Resolve only one fixed child of the already pinned checkout. O_DIRECTORY
/// and O_NOFOLLOW make a competing .delta symlink or special file an error.
fn project_directory(root: &File, create: bool) -> Result<Option<File>, String> {
    child_directory(root, ".delta", create)
}

fn child_directory(parent: &File, name: &str, create: bool) -> Result<Option<File>, String> {
    match openat(parent, name, directory_flags(), FileMode::empty()) {
        Ok(directory) => Ok(Some(File::from(directory))),
        Err(error) if error == rustix::io::Errno::NOENT && !create => Ok(None),
        Err(error) if error == rustix::io::Errno::NOENT => {
            match mkdirat(parent, name, FileMode::RWXU) {
                Ok(()) => (),
                Err(error) if error == rustix::io::Errno::EXIST => (),
                Err(error) => {
                    return Err(format!(
                        "Cannot create descriptor-relative Delta settings directory: {error}"
                    ));
                }
            }
            openat(parent, name, directory_flags(), FileMode::empty())
                .map(File::from)
                .map(Some)
                .map_err(|error| {
                    format!("Delta settings directory must be a non-symlink directory: {error}")
                })
        }
        Err(error) => Err(format!(
            "Delta settings directory must be a non-symlink directory: {error}"
        )),
    }
}

fn project_document(root: &File) -> Result<Option<Map<String, Value>>, String> {
    let Some(directory) = project_directory(root, false)? else {
        return Ok(None);
    };
    document_in(&directory)
}

fn document_in(directory: &File) -> Result<Option<Map<String, Value>>, String> {
    let Some(value) = read_json_at(directory, "settings.json")? else {
        return Ok(None);
    };
    value
        .as_object()
        .cloned()
        .map(Some)
        .ok_or_else(|| "Delta project settings must contain a JSON object".into())
}

fn trusted_store_is_separate(home: &Path, root: &Path) -> Result<(), String> {
    let home = home
        .canonicalize()
        .map_err(|error| format!("Cannot resolve Rig home for trusted Delta policy: {error}"))?;
    if home.starts_with(root) {
        return Err("Automatic Delta recording needs a trusted Rig home outside the checkout; use explicit per-session confirmation when the home is workspace-writable".into());
    }
    Ok(())
}

fn state_directory(home: &Path, create: bool) -> Result<Option<File>, String> {
    let home = home
        .canonicalize()
        .map_err(|error| format!("Cannot resolve Rig home for Delta policy: {error}"))?;
    let home = open_directory(&home)?;
    let Some(directory) = child_directory(&home, "delta", create)? else {
        return Ok(None);
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if directory
            .metadata()
            .map_err(|error| format!("Cannot inspect trusted Delta directory: {error}"))?
            .permissions()
            .mode()
            & 0o077
            != 0
        {
            return Err("Trusted Delta policy directory must not be group/world accessible".into());
        }
    }
    Ok(Some(directory))
}

fn read_store(directory: &File) -> Result<Store, String> {
    let Some(value) = read_json_at(directory, "policies.json")? else {
        return Ok(Store::empty());
    };
    let store: Store = serde_json::from_value(value)
        .map_err(|error| format!("Invalid trusted Delta policy store: {error}"))?;
    if store.version != VERSION || store.grants.len() > MAX_GRANTS {
        return Err("Unsupported or oversized trusted Delta policy store".into());
    }
    for grant in store.grants.values() {
        grant.policy.validate()?;
        if grant.consent_audit.trim().is_empty() || grant.consent_audit.len() > MAX_AUDIT_BYTES {
            return Err("Trusted Delta policy store has invalid consent provenance".into());
        }
    }
    Ok(store)
}

fn read_json_at(directory: &File, name: &str) -> Result<Option<Value>, String> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    let mut file = match openat(directory, name, flags, FileMode::empty()) {
        Ok(file) => File::from(file),
        Err(error) if error == rustix::io::Errno::NOENT => return Ok(None),
        Err(error) => {
            return Err(format!(
                "Cannot open non-symlink Delta policy file: {error}"
            ));
        }
    };
    let before = file
        .metadata()
        .map_err(|error| format!("Cannot inspect opened Delta policy file: {error}"))?;
    if !before.is_file() || before.len() > MAX_BYTES {
        return Err("Delta policy must be a regular non-symlink file of at most 1 MiB".into());
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Cannot read Delta policy file: {error}"))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Delta policy file exceeds 1 MiB".into());
    }
    let after = file
        .metadata()
        .map_err(|error| format!("Cannot revalidate opened Delta policy file: {error}"))?;
    let current = File::from(
        openat(directory, name, flags, FileMode::empty())
            .map_err(|error| format!("Cannot revalidate non-symlink Delta policy file: {error}"))?,
    );
    let current = current
        .metadata()
        .map_err(|error| format!("Cannot inspect named Delta policy file: {error}"))?;
    if !same_file(&before, &current)
        || !same_file(&before, &after)
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
    {
        return Err("Delta policy file changed while reading it".into());
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| format!("Invalid Delta policy JSON: {error}"))
}

fn same_file(first: &Metadata, second: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        first.dev() == second.dev() && first.ino() == second.ino()
    }
    #[cfg(not(unix))]
    {
        first.len() == second.len() && first.modified().ok() == second.modified().ok()
    }
}

struct StoreLock(File);
impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = unlinkat(&self.0, "policies.lock", AtFlags::empty());
    }
}

fn lock(directory: &File) -> Result<StoreLock, String> {
    let owner = directory
        .try_clone()
        .map_err(|error| format!("Cannot retain trusted Delta policy directory: {error}"))?;
    let mut file = File::from(openat(directory, "policies.lock", OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC, FileMode::RUSR | FileMode::WUSR)
        .map_err(|error| format!("Cannot lock trusted Delta policies: {error}. After a crash, inspect the lock and confirm setup is inactive before removing only the lock."))?);
    let lock = StoreLock(owner);
    writeln!(file, "process={}", std::process::id())
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("Cannot save trusted Delta policy lock: {error}"))?;
    Ok(lock)
}

fn bounded_value(value: &impl Serialize) -> Result<(), String> {
    serde_json::to_writer(
        &mut LimitedWriter {
            bytes: 0,
            file: None,
        },
        value,
    )
    .map_err(|error| format!("Delta policy exceeds its 1 MiB bound: {error}"))
}

struct TemporaryEntry<'a> {
    directory: &'a File,
    name: String,
    committed: bool,
}

impl Drop for TemporaryEntry<'_> {
    fn drop(&mut self) {
        if !self.committed {
            let _ = unlinkat(self.directory, self.name.as_str(), AtFlags::empty());
        }
    }
}

/// Creation and replacement both use the same pinned parent. A target symlink
/// introduced after validation is replaced by renameat, never followed.
fn write_json_at(directory: &File, name: &str, value: &impl Serialize) -> Result<(), String> {
    let temporary_name = format!(".rig-policy-{}.tmp", AgentId::default().0);
    let mut file = File::from(
        openat(
            directory,
            temporary_name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            FileMode::RUSR | FileMode::WUSR,
        )
        .map_err(|error| {
            format!("Cannot create descriptor-relative Delta policy transaction: {error}")
        })?,
    );
    let mut temporary = TemporaryEntry {
        directory,
        name: temporary_name,
        committed: false,
    };
    serde_json::to_writer(
        &mut LimitedWriter {
            bytes: 0,
            file: Some(&mut file),
        },
        value,
    )
    .map_err(|error| format!("Cannot write bounded Delta policy transaction: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("Cannot flush Delta policy transaction: {error}"))?;
    renameat(directory, temporary.name.as_str(), directory, name).map_err(|error| {
        format!("Cannot commit descriptor-relative Delta policy transaction: {error}")
    })?;
    temporary.committed = true;
    directory
        .sync_all()
        .map_err(|error| format!("Cannot flush Delta policy directory: {error}"))?;
    Ok(())
}

struct LimitedWriter<'a> {
    bytes: u64,
    file: Option<&'a mut File>,
}
impl Write for LimitedWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() as u64 > MAX_BYTES - self.bytes {
            return Err(std::io::Error::other("Delta policy exceeds 1 MiB"));
        }
        let written = match &mut self.file {
            Some(file) => file.write(bytes)?,
            None => bytes.len(),
        };
        self.bytes += written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match &mut self.file {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;
