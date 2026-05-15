use suprnova::data::IsRelationLoaded;
use suprnova::inertia::{Prop, PropEntry};

#[allow(non_camel_case_types)]
#[derive(suprnova::Data)]
pub struct _test_AlbumDto_t19 {
    pub id: i64,

    #[data(lazy(inertia))]
    pub songs: Prop,

    #[data(lazy(deferred))]
    pub lyrics: Prop,

    #[data(lazy(closure))]
    pub artist: Prop,
}

#[test]
fn lazy_flavors_emit_matching_prop_variants() {
    let dto = _test_AlbumDto_t19 {
        id: 1,
        songs: Prop::lazy(|| async { serde_json::json!(["s1"]) }),
        lyrics: Prop::lazy(|| async { serde_json::json!("la la") }),
        artist: Prop::lazy(|| async { serde_json::json!("Rick Astley") }),
    };
    let props = dto.__into_inertia_props();

    for (key, entry) in props {
        match (key.as_str(), &entry) {
            ("songs", PropEntry::LazyOwned { .. }) => {}
            ("lyrics", PropEntry::DeferredOwned { .. }) => {}
            ("artist", PropEntry::ClosureOwned { .. }) => {}
            ("id", PropEntry::Eager(_)) => {}
            (k, e) => panic!("unexpected entry for {k}: {e:?}"),
        }
    }
}

// Mock entity for when_loaded test
struct FakeEntity {
    relations_loaded: std::collections::HashSet<String>,
}

impl IsRelationLoaded for FakeEntity {
    fn is_relation_loaded(&self, name: &str) -> bool {
        self.relations_loaded.contains(name)
    }
}

#[test]
fn when_loaded_helper_returns_lazy_when_relation_loaded() {
    let mut entity = FakeEntity {
        relations_loaded: Default::default(),
    };
    entity.relations_loaded.insert("songs".into());

    let prop: Prop = suprnova::when_loaded!(&entity, "songs", || async {
        serde_json::json!(["s1"])
    });
    assert!(matches!(prop, Prop::Lazy(_)));
}

#[test]
fn when_loaded_helper_returns_eager_none_when_relation_unloaded() {
    let entity = FakeEntity {
        relations_loaded: Default::default(),
    };
    let prop: Prop = suprnova::when_loaded!(&entity, "songs", || async {
        serde_json::json!(["s1"])
    });
    assert!(matches!(prop, Prop::EagerNone));
}
