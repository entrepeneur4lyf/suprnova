//! db:sync command - Run migrations and sync entity files from database schema

use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

use crate::templates;
use crate::templates::{ColumnInfo, TableInfo};
use crate::ui;

pub fn run(skip_migrations: bool, regenerate_models: bool) {
    if let Err(e) = run_inner(skip_migrations, regenerate_models) {
        ui::error(&e);
        std::process::exit(1);
    }
}

fn run_inner(skip_migrations: bool, regenerate_models: bool) -> Result<(), String> {
    if !Path::new("src/models").exists() && !Path::new("src/migrations").exists() {
        return Err("Not in a Suprnova project directory".to_string());
    }

    if !skip_migrations {
        run_migrations()?;
    }

    generate_entities(regenerate_models)
}

fn run_migrations() -> Result<(), String> {
    if !Path::new("src/migrations").exists() {
        ui::warning("No migrations directory found, skipping migrations");
        return Ok(());
    }

    if !Path::new("src/bin/migrate.rs").exists() {
        ui::warning("Migration binary not found, skipping migrations");
        return Ok(());
    }

    ui::info("Running pending migrations...");

    let status = Command::new("cargo")
        .args(["run", "--quiet", "--", "migrate"])
        .status()
        .map_err(|e| format!("Failed to execute `cargo run --quiet -- migrate`: {e}"))?;

    if !status.success() {
        return Err(format!(
            "Migration failed (`cargo run --quiet -- migrate` exited with {})",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string()),
        ));
    }
    ui::success("Migrations complete");
    Ok(())
}

fn generate_entities(regenerate_models: bool) -> Result<(), String> {
    // Load DATABASE_URL from .env
    dotenvy::dotenv().ok();

    let database_url =
        env::var("DATABASE_URL").map_err(|_| "DATABASE_URL not set in .env".to_string())?;

    ui::info("Discovering database schema...");

    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| format!("Failed to start the async runtime: {e}"))?;
    rt.block_on(discover_and_generate(&database_url, regenerate_models))
}

async fn discover_and_generate(database_url: &str, regenerate_models: bool) -> Result<(), String> {
    let is_sqlite = database_url.starts_with("sqlite");

    let db = Database::connect(database_url)
        .await
        .map_err(|e| format!("Failed to connect to database: {e}"))?;

    let tables = if is_sqlite {
        discover_sqlite_tables(&db).await?
    } else {
        discover_postgres_tables(&db).await?
    };

    // Filter out migration tables
    let tables: Vec<_> = tables
        .into_iter()
        .filter(|t| t.name != "seaql_migrations" && !t.name.starts_with("_"))
        .collect();

    if tables.is_empty() {
        ui::warning("No tables found in database");
        return Ok(());
    }

    ui::success(&format!(
        "Found {} table(s): {}",
        tables.len(),
        tables
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    ));

    let models_dir = Path::new("src/models");
    if !models_dir.exists() {
        fs::create_dir_all(models_dir).map_err(|e| {
            format!(
                "Failed to create models directory {}: {e}",
                models_dir.display()
            )
        })?;
        ui::success("Created src/models directory");
    }

    let entities_dir = models_dir.join("entities");
    if !entities_dir.exists() {
        fs::create_dir_all(&entities_dir).map_err(|e| {
            format!(
                "Failed to create entities directory {}: {e}",
                entities_dir.display()
            )
        })?;
        ui::success("Created src/models/entities directory");
    }

    for table in &tables {
        generate_entity_file(table, &entities_dir)?;
        if regenerate_models {
            generate_user_file(table, models_dir)?;
        } else {
            generate_user_file_if_not_exists(table, models_dir)?;
        }
    }

    update_entities_mod(&tables, &entities_dir)?;
    update_models_mod(&tables, models_dir)?;

    ui::br();
    ui::success("Entity files generated!");
    ui::br();
    for table in &tables {
        ui::hint(&format!(
            "src/models/entities/{}.rs (auto-generated)",
            table.name
        ));
        ui::hint(&format!(
            "src/models/{}.rs (user customizations)",
            table.name
        ));
    }

    Ok(())
}

async fn discover_sqlite_tables(
    db: &sea_orm::DatabaseConnection,
) -> Result<Vec<TableInfo>, String> {
    let rows = db
        .query_all(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        ))
        .await
        .map_err(|e| format!("Failed to query sqlite_master for table list: {e}"))?;

    let table_names: Vec<String> = rows
        .iter()
        .filter_map(|row| row.try_get_by_index::<String>(0).ok())
        .collect();

    let mut tables = Vec::with_capacity(table_names.len());
    for table_name in table_names {
        let columns = discover_sqlite_columns(db, &table_name).await?;
        tables.push(TableInfo {
            name: table_name,
            columns,
        });
    }

    Ok(tables)
}

async fn discover_sqlite_columns(
    db: &sea_orm::DatabaseConnection,
    table_name: &str,
) -> Result<Vec<ColumnInfo>, String> {
    let query = format!("PRAGMA table_info(`{}`)", table_name.replace('`', "``"));
    let rows = db
        .query_all(Statement::from_string(DbBackend::Sqlite, query))
        .await
        .map_err(|e| format!("Failed to read columns for sqlite table `{table_name}`: {e}"))?;

    Ok(rows
        .iter()
        .filter_map(|row| {
            let name: String = row.try_get_by_index(1).ok()?;
            let col_type: String = row.try_get_by_index(2).ok()?;
            let notnull: i32 = row.try_get_by_index(3).ok()?;
            let pk: i32 = row.try_get_by_index(5).ok()?;

            Some(ColumnInfo {
                name,
                col_type,
                is_nullable: notnull == 0,
                is_primary_key: pk > 0,
            })
        })
        .collect())
}

async fn discover_postgres_tables(
    db: &sea_orm::DatabaseConnection,
) -> Result<Vec<TableInfo>, String> {
    let rows = db
        .query_all(Statement::from_string(
            DbBackend::Postgres,
            "SELECT table_name FROM information_schema.tables WHERE table_schema = 'public' AND table_type = 'BASE TABLE'",
        ))
        .await
        .map_err(|e| format!("Failed to query information_schema.tables: {e}"))?;

    let table_names: Vec<String> = rows
        .iter()
        .filter_map(|row| row.try_get_by_index::<String>(0).ok())
        .collect();

    let mut tables = Vec::with_capacity(table_names.len());
    for table_name in table_names {
        let columns = discover_postgres_columns(db, &table_name).await?;
        tables.push(TableInfo {
            name: table_name,
            columns,
        });
    }

    Ok(tables)
}

async fn discover_postgres_columns(
    db: &sea_orm::DatabaseConnection,
    table_name: &str,
) -> Result<Vec<ColumnInfo>, String> {
    let escaped = table_name.replace('"', "\"\"");
    let query = format!(
        r#"
        SELECT
            c.column_name,
            c.data_type,
            c.is_nullable,
            CASE WHEN pk.column_name IS NOT NULL THEN true ELSE false END as is_pk
        FROM information_schema.columns c
        LEFT JOIN (
            SELECT ku.column_name
            FROM information_schema.table_constraints tc
            JOIN information_schema.key_column_usage ku
                ON tc.constraint_name = ku.constraint_name
            WHERE tc.constraint_type = 'PRIMARY KEY'
                AND tc.table_name = "{}"
        ) pk ON c.column_name = pk.column_name
        WHERE c.table_name = "{}"
        ORDER BY c.ordinal_position
        "#,
        escaped, escaped
    );

    let rows = db
        .query_all(Statement::from_string(DbBackend::Postgres, query))
        .await
        .map_err(|e| format!("Failed to read columns for postgres table `{table_name}`: {e}"))?;

    Ok(rows
        .iter()
        .filter_map(|row| {
            let name: String = row.try_get_by_index(0).ok()?;
            let col_type: String = row.try_get_by_index(1).ok()?;
            let is_nullable_str: String = row.try_get_by_index(2).ok()?;
            let is_pk: bool = row.try_get_by_index(3).ok()?;

            Some(ColumnInfo {
                name,
                col_type,
                is_nullable: is_nullable_str == "YES",
                is_primary_key: is_pk,
            })
        })
        .collect())
}

fn generate_entity_file(table: &TableInfo, entities_dir: &Path) -> Result<(), String> {
    let entity_file = entities_dir.join(format!("{}.rs", table.name));
    let content = templates::entity_template(&table.name, &table.columns);

    fs::write(&entity_file, content)
        .map_err(|e| format!("Failed to write entity file {}: {e}", entity_file.display()))?;
    ui::success(&format!("Generated src/models/entities/{}.rs", table.name));
    Ok(())
}

fn generate_user_file_if_not_exists(table: &TableInfo, models_dir: &Path) -> Result<(), String> {
    let user_file = models_dir.join(format!("{}.rs", table.name));

    if user_file.exists() {
        ui::hint(&format!(
            "Skipped src/models/{}.rs (already exists)",
            table.name
        ));
        return Ok(());
    }

    let struct_name = to_pascal_case(&singularize(&table.name));
    let content = templates::user_model_template(&table.name, &struct_name, &table.columns);

    fs::write(&user_file, content).map_err(|e| {
        format!(
            "Failed to write user model file {}: {e}",
            user_file.display()
        )
    })?;
    ui::success(&format!("Created src/models/{}.rs", table.name));
    Ok(())
}

fn generate_user_file(table: &TableInfo, models_dir: &Path) -> Result<(), String> {
    let user_file = models_dir.join(format!("{}.rs", table.name));
    let struct_name = to_pascal_case(&singularize(&table.name));
    let content = templates::user_model_template(&table.name, &struct_name, &table.columns);

    fs::write(&user_file, content).map_err(|e| {
        format!(
            "Failed to write user model file {}: {e}",
            user_file.display()
        )
    })?;
    ui::success(&format!("Regenerated src/models/{}.rs", table.name));
    Ok(())
}

fn update_entities_mod(tables: &[TableInfo], entities_dir: &Path) -> Result<(), String> {
    let mod_file = entities_dir.join("mod.rs");
    let content = templates::entities_mod_template(tables);

    fs::write(&mod_file, content).map_err(|e| {
        format!(
            "Failed to write entities/mod.rs {}: {e}",
            mod_file.display()
        )
    })?;
    ui::success("Updated src/models/entities/mod.rs");
    Ok(())
}

fn update_models_mod(tables: &[TableInfo], models_dir: &Path) -> Result<(), String> {
    let mod_file = models_dir.join("mod.rs");

    // Read existing content (or seed default if absent).  Surface real read
    // failures — silently defaulting on EPERM/EIO would obliterate the user's
    // mod.rs on the subsequent write.
    let existing_content = if mod_file.exists() {
        fs::read_to_string(&mod_file).map_err(|e| {
            format!(
                "Failed to read existing models/mod.rs {}: {e}",
                mod_file.display()
            )
        })?
    } else {
        "//! Application models\n\n".to_string()
    };

    let mut lines: Vec<String> = existing_content.lines().map(String::from).collect();

    let has_entities_mod = lines.iter().any(|l| {
        let trimmed = l.trim();
        trimmed == "pub mod entities;" || trimmed == "mod entities;"
    });

    let mut insert_idx = 0;
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("//!") || line.is_empty() {
            insert_idx = i + 1;
        } else {
            break;
        }
    }

    if !has_entities_mod {
        lines.insert(insert_idx, "pub mod entities;".to_string());
        insert_idx += 1;
    }

    for table in tables {
        let mod_decl = format!("pub mod {};", table.name);
        let alt_mod_decl = format!("mod {};", table.name);

        if !lines
            .iter()
            .any(|l| l.trim() == mod_decl || l.trim() == alt_mod_decl)
        {
            let mut last_mod_idx = insert_idx;
            for (i, line) in lines.iter().enumerate() {
                if line.trim().starts_with("pub mod ") || line.trim().starts_with("mod ") {
                    last_mod_idx = i + 1;
                }
            }
            lines.insert(last_mod_idx, mod_decl);
        }
    }

    let content = lines.join("\n") + "\n";
    fs::write(&mod_file, content)
        .map_err(|e| format!("Failed to write models/mod.rs {}: {e}", mod_file.display()))?;
    ui::success("Updated src/models/mod.rs");
    Ok(())
}

fn to_pascal_case(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = true;

    for c in s.chars() {
        if c == '_' || c == '-' || c == ' ' {
            capitalize_next = true;
        } else if capitalize_next {
            // `char::to_uppercase` yields at least one char by the std::char
            // documented contract — this `.next()` is infallible on any char.
            result.push(c.to_uppercase().next().unwrap_or(c));
            capitalize_next = false;
        } else {
            result.push(c);
        }
    }
    result
}

fn singularize(word: &str) -> String {
    // Basic singularization
    if let Some(stem) = word.strip_suffix("ies") {
        format!("{}y", stem)
    } else if word.ends_with("es") && !word.ends_with("ses") && !word.ends_with("xes") {
        word[..word.len() - 2].to_string()
    } else if word.ends_with("s") && !word.ends_with("ss") && !word.ends_with("us") {
        word[..word.len() - 1].to_string()
    } else {
        word.to_string()
    }
}
