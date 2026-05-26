//! Asserts that every SeaORM type a consumer would touch is re-exported
//! under `suprnova::*` so app code never needs `use sea_orm::*`.

// `DeriveActiveEnum` is a derive macro (not a type) — verify it via
// an actual derive use on a probe enum below.
use suprnova::DeriveActiveEnum;
#[derive(
    ::std::clone::Clone,
    ::std::fmt::Debug,
    ::std::marker::Copy,
    PartialEq,
    Eq,
    ::suprnova::sea_orm::EnumIter,
    DeriveActiveEnum,
)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
enum _DeriveActiveEnumProbe {
    #[sea_orm(num_value = 1)]
    A,
    #[sea_orm(num_value = 2)]
    B,
}

#[test]
fn sea_orm_types_are_aliased_under_suprnova() {
    // Structs / values. `Set` and `NotSet` are variants of `ActiveValue`
    // (re-exported as bare names by SeaORM), so they're verified by
    // constructing values — not by type position.
    use suprnova::{
        ActiveValue, DatabaseConnection, DatabaseTransaction, NotSet, RelationDef, Schema, Select,
        Set,
    };
    let _set: ActiveValue<i64> = Set(7);
    let _not_set: ActiveValue<i64> = NotSet;
    let _ = std::marker::PhantomData::<(
        DatabaseConnection,
        DatabaseTransaction,
        ActiveValue<i64>,
        RelationDef,
        Schema,
    )>;
    // `Select<E>` requires `E: EntityTrait`. Trait-bound check fn proves
    // the re-export resolves without naming a concrete entity here.
    fn _t_select<E: suprnova::EntityTrait>() -> Option<Select<E>> {
        None
    }

    // Traits — verified via trait-bound check fns. Bare trait names
    // can't appear as types; `<T: Trait>()` checks the trait resolves
    // at compile time which is what we want here.
    fn _t_column<T: suprnova::ColumnTrait>() {}
    fn _t_conn<T: suprnova::ConnectionTrait>() {}
    fn _t_active_model<T: suprnova::ActiveModelTrait>() {}
    fn _t_active_behavior<T: suprnova::ActiveModelBehavior>() {}
    fn _t_model<T: suprnova::ModelTrait>() {}
    fn _t_query_filter<T: suprnova::QueryFilter>() {}
    fn _t_query_order<T: suprnova::QueryOrder>() {}
    fn _t_query_select<T: suprnova::QuerySelect>() {}
    fn _t_transaction<T: suprnova::TransactionTrait>() {}
    fn _t_iden<T: suprnova::Iden>() {}
    fn _t_entity_name<T: suprnova::EntityName>() {}
    fn _t_entity_trait<T: suprnova::EntityTrait>() {}
    fn _t_primary_key<T: suprnova::PrimaryKeyTrait>() {}
    // `PrimaryKeyToColumn::Column: ColumnTrait` — `()` doesn't satisfy
    // that. Without the associated-type binding the trait re-export
    // still resolves which is what this test exercises.
    fn _t_pk_to_column<T: suprnova::PrimaryKeyToColumn>() {}
    // `IntoActiveModel<A>` requires `A: ActiveModelTrait`, which `()`
    // doesn't satisfy. The trait re-export is what we're checking, so
    // a bound without the associated-type binding exercises the path.
    fn _t_into_active_model<A: suprnova::ActiveModelTrait, T: suprnova::IntoActiveModel<A>>() {}
    fn _t_relation_trait<T: suprnova::RelationTrait>() {}
    fn _t_iterable<I: suprnova::Iterable>() {}

    // Module access: `suprnova::sea_query` must resolve.
    let _ = std::marker::PhantomData::<suprnova::sea_query::Alias>;

    // Touch the DeriveActiveEnum probe to defeat dead-code elimination.
    let _ = _DeriveActiveEnumProbe::A;
}
