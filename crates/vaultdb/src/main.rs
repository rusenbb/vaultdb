mod cli;
mod commands;
mod output;

use std::process;

use anyhow::Result;
use clap::Parser;

use cli::{Cli, Command};
use vaultdb_core::vault::Vault;

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {:#}", e);
        process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    let vault = match &cli.vault {
        Some(path) => Vault::with_root(path.clone()),
        None => {
            let cwd = std::env::current_dir()?;
            Vault::discover(&cwd)?
        }
    };

    match &cli.command {
        Command::Query {
            folder,
            where_exprs,
            select,
            sort,
            desc,
            limit,
            format,
            links_to,
            linked_from,
            links_to_where,
            linked_from_where,
            output,
            csv_delimiter,
        } => {
            let relational = commands::query::RelationalFilters {
                links_to: links_to.clone(),
                linked_from: linked_from.clone(),
                links_to_where: links_to_where.clone(),
                linked_from_where: linked_from_where.clone(),
            };
            commands::query::run_query(
                &vault,
                folder,
                where_exprs,
                select,
                sort.as_deref(),
                *desc,
                *limit,
                format,
                &relational,
                cli.recursive,
                cli.verbose,
                output.as_deref(),
                *csv_delimiter,
            )
        }

        Command::Count {
            folder,
            where_exprs,
        } => commands::query::run_count(&vault, folder, where_exprs, cli.recursive, cli.verbose),

        Command::Fields { folder } => {
            commands::query::run_fields(&vault, folder, cli.recursive, cli.verbose)
        }

        Command::Tags { folder } => {
            commands::query::run_tags(&vault, folder, cli.recursive, cli.verbose)
        }

        Command::Unresolved {
            folder,
            from,
            depth,
        } => commands::unresolved::run_unresolved(
            &vault,
            folder,
            from.as_deref(),
            *depth,
            cli.recursive,
            cli.verbose,
        ),

        Command::Links {
            name,
            folder,
            direction,
        } => {
            commands::links::run_links(&vault, name, folder, direction, cli.recursive, cli.verbose)
        }

        Command::Traverse {
            name,
            folder,
            depth,
            direction,
            where_exprs,
            select,
        } => commands::traverse::run_traverse(
            &vault,
            name,
            folder,
            *depth,
            direction,
            where_exprs,
            select,
            cli.recursive,
            cli.verbose,
        ),

        Command::Create {
            folder,
            name,
            template,
            set,
            body,
        } => commands::create::run_create(
            &vault,
            folder,
            name,
            template.as_deref(),
            set,
            body.as_deref(),
            cli.dry_run,
        ),

        Command::Rename {
            old_name,
            new_name,
            folder,
        } => commands::rename::run_rename(
            &vault,
            old_name,
            new_name,
            folder,
            cli.dry_run,
            cli.verbose,
        ),

        Command::Update {
            folder,
            where_exprs,
            set,
            unset,
            add_tag,
            remove_tag,
            set_body,
            append_body,
            clear_body,
            body_separator,
        } => {
            let ops = commands::update::parse_operations(
                set,
                unset,
                add_tag,
                remove_tag,
                set_body.as_deref(),
                append_body,
                *clear_body,
            )?;
            let sep = body_separator
                .as_deref()
                .map(commands::update::unescape_separator);
            commands::update::run_update(
                &vault,
                folder,
                where_exprs,
                &ops,
                sep.as_deref(),
                cli.dry_run,
                cli.recursive,
                cli.verbose,
            )
        }

        Command::Move {
            folder,
            where_exprs,
            to,
        } => commands::move_cmd::run_move(
            &vault,
            folder,
            where_exprs,
            to,
            cli.dry_run,
            cli.recursive,
            cli.verbose,
        ),

        Command::Delete {
            folder,
            where_exprs,
            force,
        } => commands::delete::run_delete(
            &vault,
            folder,
            where_exprs,
            *force,
            cli.dry_run,
            cli.recursive,
            cli.verbose,
        ),

        Command::Schema { action } => match action {
            cli::SchemaAction::Show { folder } => commands::schema_cmd::run_show(&vault, folder),
            cli::SchemaAction::Validate { folder } => {
                commands::schema_cmd::run_validate(&vault, folder, cli.recursive, cli.verbose)
            }
            cli::SchemaAction::Init { folder, write } => {
                commands::schema_cmd::run_init(&vault, folder, cli.recursive, cli.verbose, *write)
            }
        },
    }
}
