use serde::{Deserialize, Serialize};
use suprnova::data::registry;

#[derive(suprnova::Data)]
pub struct Paginated<T>
where
    T: Serialize + for<'de> Deserialize<'de>,
{
    pub items: Vec<T>,
    pub total: usize,

    #[data(allow_include)]
    pub meta: Option<serde_json::Value>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Item {
    pub id: i64,
    pub label: String,
}

#[test]
fn generic_struct_serializes() {
    let p: Paginated<Item> = Paginated {
        items: vec![Item {
            id: 1,
            label: "x".into(),
        }],
        total: 1,
        meta: None,
    };
    let j = serde_json::to_value(&p).unwrap();
    assert_eq!(j["total"], 1);
    assert_eq!(j["items"][0]["label"], "x");
}

#[test]
fn generic_struct_deserializes() {
    let raw = serde_json::json!({
        "items": [{"id": 1, "label": "x"}],
        "total": 1,
        "meta": null,
    });
    let p: Paginated<Item> = serde_json::from_value(raw).unwrap();
    assert_eq!(p.total, 1);
    assert_eq!(p.items.len(), 1);
    assert!(p.meta.is_none());
}

#[test]
fn allowlist_keyed_by_fully_qualified_type_name() {
    // Audit HIGH #336: previously the registry was keyed by the bare
    // struct name ("Paginated"), so two DTOs with the same identifier
    // in different modules would silently overwrite each other.
    // Post-fix the key is the fully-qualified type name produced by
    // `concat!(module_path!(), "::", stringify!(StructName))`.
    let qualified = ::std::concat!(::std::module_path!(), "::", "Paginated");
    assert!(
        registry::is_allowed(qualified, "meta"),
        "fully-qualified type name must be the registry key; tried `{qualified}`"
    );
    assert!(!registry::is_allowed(qualified, "items"));

    // Bare struct names must NOT match — that was the collision-prone
    // contract this audit finding closed.
    assert!(!registry::is_allowed("Paginated", "meta"));
    // Per-instantiation keys must not match either — generic
    // instantiations share the type's allowlist.
    assert!(!registry::is_allowed("Paginated<Item>", "meta"));
}

#[test]
fn generic_with_lifetime_param_compiles() {
    #[derive(suprnova::Data)]
    pub struct Borrowed<'a, T: Serialize + for<'de> Deserialize<'de>> {
        pub inner: &'a T,
    }

    let s = Item {
        id: 7,
        label: "k".into(),
    };
    let b = Borrowed { inner: &s };
    let j = serde_json::to_value(&b).unwrap();
    assert_eq!(j["inner"]["id"], 7);
}

#[test]
fn two_distinct_instantiations_in_same_file_compile() {
    let with_item: Paginated<Item> = Paginated {
        items: vec![Item {
            id: 1,
            label: "x".into(),
        }],
        total: 1,
        meta: None,
    };
    let with_string: Paginated<String> = Paginated {
        items: vec!["a".into(), "b".into()],
        total: 2,
        meta: None,
    };

    let j1 = serde_json::to_value(&with_item).unwrap();
    let j2 = serde_json::to_value(&with_string).unwrap();
    assert_eq!(j1["items"][0]["label"], "x");
    assert_eq!(j2["items"][0], "a");

    let _back1: Paginated<Item> = serde_json::from_value(j1).unwrap();
    let _back2: Paginated<String> = serde_json::from_value(j2).unwrap();
}
