//! Shared-catalog contracts consumed by ACP and the terminal model picker.

use rig_core::{
    catalog::{Connector, ModelSpec, Transport},
    completion::Effort,
    operation::Completion,
    providers::registry::ProviderId,
    serve::{ErasedHandler, adapters::ModelAdapter},
    test_utils::MockCompletionModel,
};

use super::*;

#[test]
fn registers_provider_specific_models_without_changing_builtin_definitions() {
    let mut app = App::new();
    app.add_plugins(ModelCatalogPlugin);
    let models = app.world().resource::<Models>();
    let catalog = models.0.catalog();
    let builtin = Catalog::builtin();
    for original in builtin.iter() {
        assert_eq!(
            catalog.get_exact(original.provider, &original.id),
            Some(original),
            "{}",
            original.reference()
        );
    }
    let additions = Catalog::from_overrides(MODELS, &Catalog::default()).unwrap();
    let mut missing = 0;
    for spec in additions.iter() {
        let existing = builtin.get_exact(spec.provider, &spec.id);
        missing += usize::from(existing.is_none());
        assert_eq!(
            catalog.get_exact(spec.provider, &spec.id),
            Some(existing.unwrap_or(spec)),
            "{}",
            spec.reference()
        );
    }
    assert_eq!(catalog.iter().count(), builtin.iter().count() + missing);

    // Regression checks for the initial additions; registration above covers
    // every bundled row, so new providers need no loader or count changes.
    for (reference, name, output, input_price, output_price) in [
        (
            "gcp.gemini/gemini-4-carbon",
            "Gemini 4 Carbon",
            32_768,
            2.0,
            10.0,
        ),
        (
            "gcp.gemini/gemini-4-hydrogen",
            "Gemini 4 Hydrogen",
            65_536,
            1.5,
            7.5,
        ),
        (
            "vertexai/gemini-4-carbon",
            "Gemini 4 Carbon",
            65_536,
            2.0,
            10.0,
        ),
        (
            "vertexai/gemini-4-hydrogen",
            "Gemini 4 Hydrogen",
            65_536,
            1.5,
            7.5,
        ),
        (
            "openrouter/google/gemini-4-carbon",
            "Gemini 4 Carbon",
            943_718,
            2.0,
            10.0,
        ),
        (
            "openrouter/google/gemini-4-hydrogen",
            "Gemini 4 Hydrogen",
            943_718,
            1.5,
            7.5,
        ),
        (
            "copilot/gemini-4-carbon",
            "Gemini 4 Carbon",
            65_536,
            2.0,
            10.0,
        ),
    ] {
        let spec = catalog.resolve(reference).unwrap().spec;
        assert_eq!(spec.reference(), reference);
        assert_eq!(spec.display_name, name);
        assert_eq!(spec.context_window, Some(1_048_576));
        assert_eq!(spec.max_output_tokens, Some(output));
        assert!(spec.tools && spec.structured_output);
        let pricing = spec.pricing.as_ref().unwrap();
        assert_eq!((pricing.input, pricing.output), (input_price, output_price));
    }
}

#[test]
fn reasoning_choices_respect_the_pinned_gemini_translator_boundary() {
    let mut app = App::new();
    app.add_plugins(ModelCatalogPlugin);
    let models = app.world().resource::<Models>();
    let catalog = models.0.catalog();
    for reference in [
        "gcp.gemini/gemini-4-carbon",
        "gcp.gemini/gemini-4-hydrogen",
        "openrouter/google/gemini-4-carbon",
    ] {
        assert_eq!(
            catalog.resolve(reference).unwrap().spec.reasoning.levels(),
            Some([Effort::Low, Effort::Medium, Effort::High].as_slice()),
            "{reference}"
        );
    }
    assert_eq!(
        catalog
            .resolve("openrouter/google/gemini-4-hydrogen")
            .unwrap()
            .spec
            .reasoning
            .levels(),
        Some([Effort::Low, Effort::Medium, Effort::High, Effort::XHigh].as_slice())
    );
}

#[test]
fn preserves_custom_rows_and_handlers_in_the_reachable_list_acp_reads() {
    let google = ProviderId::catalog("gcp.gemini").unwrap();
    let custom = ModelSpec::new(google, "gemini-4-carbon")
        .with_tools(true)
        .with_display_name("My Carbon definition")
        .with_context_window(123_456);
    let anthropic = ProviderId::catalog("anthropic").unwrap();
    let unrelated = ModelSpec::new(anthropic, "existing-model").with_tools(true);
    let mut connector = Connector::new(Catalog::default());
    connector.catalog_mut().insert(custom.clone());
    connector.catalog_mut().insert(unrelated.clone());
    for provider in [google, anthropic] {
        connector.provide(provider, |spec: &ModelSpec, _: &Catalog, _: &Transport| {
            Ok(ErasedHandler::new(ModelAdapter::<Completion>::new(
                spec.reference(),
                MockCompletionModel::text("unused; no requests are sent"),
            )))
        });
    }
    let mut app = App::new();
    app.insert_resource(Models(connector))
        .add_plugins(ModelCatalogPlugin);
    let models = app.world().resource::<Models>();
    assert!(models.0.is_provided(google));
    assert!(models.0.is_provided(anthropic));
    for spec in [&custom, &unrelated] {
        assert_eq!(
            models.0.catalog().get_exact(spec.provider, &spec.id),
            Some(spec)
        );
    }
    // Registration must not install providers or turn catalog data into access.
    for vendor in ["vertexai", "openrouter", "copilot"] {
        assert!(!models.0.is_provided(ProviderId::catalog(vendor).unwrap()));
    }
    let reachable = models.0.reachable();
    for (provider, id) in [
        (google, "gemini-4-carbon"),
        (google, "gemini-4-hydrogen"),
        (anthropic, "existing-model"),
    ] {
        assert!(
            reachable
                .iter()
                .any(|spec| spec.provider == provider && spec.id == id)
        );
    }
}
