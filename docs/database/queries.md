---
title: 'Query Builder'
description: 'Build complex database queries with SeaORM'
icon: 'magnifying-glass'
---

suprnova uses SeaORM's powerful query builder for complex database operations. While the `Model` and `ModelMut` traits handle common operations, you'll use the query builder for filtering, sorting, pagination, and joins.

## Basic Queries

### Getting the Connection

Always start by getting the database connection:

```rust
use suprnova::database::DB;

let db = DB::connection();
```

### Find All

```rust
use sea_orm::EntityTrait;
use crate::models::todos::Entity as Todos;

let todos = Todos::find()
    .all(DB::connection())
    .await?;
```

### Find by ID

```rust
let todo = Todos::find_by_id(1)
    .one(DB::connection())
    .await?;

// Returns Option<Model>
if let Some(todo) = todo {
    println!("Found: {}", todo.title);
}
```

## Filtering with Conditions

Use the `filter()` method with `Condition` for complex queries:

```rust
use sea_orm::{EntityTrait, QueryFilter, ColumnTrait};
use crate::models::todos::{Entity as Todos, Column};

// Simple equality
let completed_todos = Todos::find()
    .filter(Column::Completed.eq(true))
    .all(DB::connection())
    .await?;

// Multiple conditions
let recent_completed = Todos::find()
    .filter(Column::Completed.eq(true))
    .filter(Column::Title.contains("urgent"))
    .all(DB::connection())
    .await?;
```

### Available Filter Methods

| Method | SQL Equivalent | Example |
|--------|----------------|---------|
| `eq(value)` | `= value` | `Column::Status.eq("active")` |
| `ne(value)` | `!= value` | `Column::Status.ne("deleted")` |
| `gt(value)` | `> value` | `Column::Age.gt(18)` |
| `gte(value)` | `>= value` | `Column::Age.gte(21)` |
| `lt(value)` | `< value` | `Column::Price.lt(100)` |
| `lte(value)` | `<= value` | `Column::Price.lte(50)` |
| `between(a, b)` | `BETWEEN a AND b` | `Column::Age.between(18, 65)` |
| `like(pattern)` | `LIKE pattern` | `Column::Name.like("%john%")` |
| `starts_with(s)` | `LIKE 's%'` | `Column::Name.starts_with("J")` |
| `ends_with(s)` | `LIKE '%s'` | `Column::Email.ends_with(".com")` |
| `contains(s)` | `LIKE '%s%'` | `Column::Title.contains("rust")` |
| `is_null()` | `IS NULL` | `Column::DeletedAt.is_null()` |
| `is_not_null()` | `IS NOT NULL` | `Column::Email.is_not_null()` |
| `is_in(vec)` | `IN (...)` | `Column::Status.is_in(vec!["a", "b"])` |
| `is_not_in(vec)` | `NOT IN (...)` | `Column::Id.is_not_in(vec![1, 2])` |

## Complex Conditions

### OR Conditions

```rust
use sea_orm::{Condition, EntityTrait, QueryFilter, ColumnTrait};

let todos = Todos::find()
    .filter(
        Condition::any()
            .add(Column::Title.contains("urgent"))
            .add(Column::Title.contains("important"))
    )
    .all(DB::connection())
    .await?;
```

### AND + OR Combined

```rust
let todos = Todos::find()
    .filter(
        Condition::all()
            .add(Column::Completed.eq(false))
            .add(
                Condition::any()
                    .add(Column::Title.contains("urgent"))
                    .add(Column::Priority.gt(5))
            )
    )
    .all(DB::connection())
    .await?;
```

## Sorting

### Order By

```rust
use sea_orm::{EntityTrait, QueryOrder};

// Single column
let todos = Todos::find()
    .order_by_asc(Column::CreatedAt)
    .all(DB::connection())
    .await?;

// Descending
let recent_first = Todos::find()
    .order_by_desc(Column::CreatedAt)
    .all(DB::connection())
    .await?;

// Multiple columns
let sorted = Todos::find()
    .order_by_desc(Column::Priority)
    .order_by_asc(Column::Title)
    .all(DB::connection())
    .await?;
```

## Pagination

### Limit and Offset

```rust
use sea_orm::{EntityTrait, QuerySelect};

// Get first 10
let page_1 = Todos::find()
    .limit(10)
    .all(DB::connection())
    .await?;

// Get page 2 (items 11-20)
let page_2 = Todos::find()
    .limit(10)
    .offset(10)
    .all(DB::connection())
    .await?;
```

### Paginator

SeaORM provides a built-in paginator:

```rust
use sea_orm::{EntityTrait, PaginatorTrait};

let paginator = Todos::find()
    .order_by_desc(Column::CreatedAt)
    .paginate(DB::connection(), 10);  // 10 items per page

// Get total pages
let num_pages = paginator.num_pages().await?;

// Get specific page (0-indexed)
let page_0 = paginator.fetch_page(0).await?;
let page_1 = paginator.fetch_page(1).await?;
```

## Selecting Specific Columns

### Partial Select

```rust
use sea_orm::{EntityTrait, QuerySelect};

#[derive(FromQueryResult)]
struct TodoTitle {
    id: i32,
    title: String,
}

let titles = Todos::find()
    .select_only()
    .column(Column::Id)
    .column(Column::Title)
    .into_model::<TodoTitle>()
    .all(DB::connection())
    .await?;
```

## Aggregations

### Count

```rust
use sea_orm::{EntityTrait, PaginatorTrait};

let count = Todos::find()
    .filter(Column::Completed.eq(true))
    .count(DB::connection())
    .await?;
```

### Sum, Avg, Min, Max

```rust
use sea_orm::{EntityTrait, QuerySelect, sea_query::Func};

// Using raw expressions for aggregations
let result = Todos::find()
    .select_only()
    .column_as(Column::Priority.sum(), "total_priority")
    .into_tuple::<(Option<i32>,)>()
    .one(DB::connection())
    .await?;
```

## Joins

### Loading Related Data

```rust
use sea_orm::EntityTrait;
use crate::models::{posts, users};

// Load post with its author
let post_with_author = posts::Entity::find_by_id(1)
    .find_also_related(users::Entity)
    .one(DB::connection())
    .await?;

// Load all posts for a user
let user_posts = users::Entity::find_by_id(1)
    .find_with_related(posts::Entity)
    .all(DB::connection())
    .await?;
```

### Custom Joins

```rust
use sea_orm::{EntityTrait, QuerySelect, JoinType, RelationTrait};

let results = posts::Entity::find()
    .join(JoinType::InnerJoin, posts::Relation::User.def())
    .filter(users::Column::Active.eq(true))
    .all(DB::connection())
    .await?;
```

## Raw Queries

For complex queries that can't be expressed with the query builder:

```rust
use sea_orm::{FromQueryResult, DbBackend, Statement};

#[derive(FromQueryResult)]
struct CustomResult {
    count: i64,
    category: String,
}

let results: Vec<CustomResult> = CustomResult::find_by_statement(
    Statement::from_sql_and_values(
        DbBackend::Sqlite,
        r#"SELECT COUNT(*) as count, category FROM todos GROUP BY category"#,
        vec![],
    )
)
.all(DB::connection())
.await?;
```

## Transactions

### Basic Transaction

```rust
use sea_orm::TransactionTrait;

let result = DB::connection()
    .transaction::<_, (), sea_orm::DbErr>(|txn| {
        Box::pin(async move {
            // All operations in this block use the transaction
            let todo = ActiveModel {
                title: Set("New task".to_string()),
                ..Default::default()
            };
            Todos::insert(todo).exec(txn).await?;

            // More operations...

            Ok(())  // Commit
            // Return Err(...) to rollback
        })
    })
    .await;
```

## Query Examples

### Search with Pagination

```rust
pub async fn search_todos(
    query: &str,
    page: u64,
    per_page: u64,
) -> Result<(Vec<Todo>, u64), DbErr> {
    let paginator = Todos::find()
        .filter(
            Condition::any()
                .add(Column::Title.contains(query))
                .add(Column::Description.contains(query))
        )
        .order_by_desc(Column::CreatedAt)
        .paginate(DB::connection(), per_page);

    let total_pages = paginator.num_pages().await?;
    let todos = paginator.fetch_page(page).await?;

    Ok((todos, total_pages))
}
```

### Soft Delete Pattern

```rust
// Only get non-deleted records
let active_todos = Todos::find()
    .filter(Column::DeletedAt.is_null())
    .all(DB::connection())
    .await?;

// Soft delete a record
let mut todo: ActiveModel = existing_todo.into();
todo.deleted_at = Set(Some(chrono::Utc::now().naive_utc()));
Todos::update_one(todo).await?;
```

### Conditional Query Building

```rust
pub async fn list_todos(
    completed: Option<bool>,
    search: Option<String>,
    sort_by: Option<String>,
) -> Result<Vec<Todo>, DbErr> {
    let mut query = Todos::find();

    // Apply filters conditionally
    if let Some(completed) = completed {
        query = query.filter(Column::Completed.eq(completed));
    }

    if let Some(search) = search {
        query = query.filter(Column::Title.contains(&search));
    }

    // Apply sorting
    query = match sort_by.as_deref() {
        Some("title") => query.order_by_asc(Column::Title),
        Some("created") => query.order_by_desc(Column::CreatedAt),
        _ => query.order_by_desc(Column::Id),
    };

    query.all(DB::connection()).await
}
```

## Summary

| Operation | Method | Example |
|-----------|--------|---------|
| Find all | `find().all()` | `Entity::find().all(db)` |
| Find by ID | `find_by_id()` | `Entity::find_by_id(1).one(db)` |
| Filter | `filter()` | `find().filter(Column::X.eq(y))` |
| Sort | `order_by_*()` | `find().order_by_asc(Column::X)` |
| Limit | `limit()` | `find().limit(10)` |
| Offset | `offset()` | `find().offset(20)` |
| Paginate | `paginate()` | `find().paginate(db, 10)` |
| Count | `count()` | `find().count(db)` |
| Join | `find_also_related()` | `find().find_also_related(Other)` |
