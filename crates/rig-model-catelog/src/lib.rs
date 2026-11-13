//! Day-one model definitions for Rig's shared ACP and terminal catalog.
//!
//! Registration happens before session startup and preserves every existing
//! definition, provider handler, sign-in and transport. This plugin supplies
//! catalog data only; it neither connects to providers nor installs them.

use rig_harness::prelude::*;
use rig_harness::rig_core::catalog::Catalog;

const MODELS: &str = include_str!("models.json");

/// Adds missing model rows from the bundled provider-neutral supplement.
///
/// Existing rows win, including user definitions and newer upstream entries.
/// Authentication and reachability remain the existing connector's decision.
#[derive(Default, Debug)]
pub struct ModelCatalogPlugin;

impl Plugin for ModelCatalogPlugin {
    fn build(&self, app: &mut App) {
        // Validate the complete embedded supplement before changing host state.
        let additions = Catalog::from_overrides(MODELS, &Catalog::default())
            .expect("the bundled model catalog must be a valid Rig 0.46.0 override");
        let mut models = app.world_mut().get_resource_or_init::<Models>();
        let catalog = models.0.catalog_mut();
        for spec in additions.iter() {
            if catalog.get_exact(spec.provider, &spec.id).is_none() {
                catalog.insert(spec.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests;
