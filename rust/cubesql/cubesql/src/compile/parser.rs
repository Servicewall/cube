use std::collections::HashMap;

use regex::Regex;
use sqlparser::{
    ast::Statement,
    dialect::{Dialect, PostgreSqlDialect},
    parser::Parser,
};

use crate::{
    compile::{qtrace::Qtrace, CompilationError},
    sql::session::DatabaseProtocol,
};

use super::CompilationResult;

#[derive(Debug)]
pub struct MySqlDialectWithBackTicks {}

impl Dialect for MySqlDialectWithBackTicks {
    fn is_delimited_identifier_start(&self, ch: char) -> bool {
        ch == '"' || ch == '`'
    }

    fn is_identifier_start(&self, ch: char) -> bool {
        // See https://dev.mysql.com/doc/refman/8.0/en/identifiers.html.
        // We don't yet support identifiers beginning with numbers, as that
        // makes it hard to distinguish numeric literals.
        ('a'..='z').contains(&ch)
            || ('A'..='Z').contains(&ch)
            || ch == '_'
            || ch == '$'
            || ch == '@'
            || ('\u{0080}'..='\u{ffff}').contains(&ch)
    }

    fn is_identifier_part(&self, ch: char) -> bool {
        self.is_identifier_start(ch) || ('0'..='9').contains(&ch)
    }
}

lazy_static! {
    static ref SIGMA_WORKAROUND: Regex = Regex::new(r#"(?s)^\s*with\s+nsp\sas\s\(.*nspname\s=\s.*\),\s+tbl\sas\s\(.*relname\s=\s.*\).*select\s+attname.*from\spg_attribute.*$"#).unwrap();
}

pub fn parse_sql_to_statements(
    query: &String,
    protocol: DatabaseProtocol,
    qtrace: &mut Option<Qtrace>,
) -> CompilationResult<Vec<Statement>> {
    let original_query = query.clone();

    log::debug!("Parsing SQL: {}", query);
    // @todo Support without workarounds
    // metabase
    let query = query.clone().replace("IF(TABLE_TYPE='BASE TABLE' or TABLE_TYPE='SYSTEM VERSIONED', 'TABLE', TABLE_TYPE) as TABLE_TYPE", "TABLE_TYPE");
    let query = query.replace("ORDER BY TABLE_TYPE, TABLE_SCHEMA, TABLE_NAME", "");
    // @todo Implement CONVERT function
    let query = query.replace("CONVERT (CASE DATA_TYPE WHEN 'year' THEN NUMERIC_SCALE WHEN 'tinyint' THEN 0 ELSE NUMERIC_SCALE END, UNSIGNED INTEGER)", "0");
    // @todo problem with parser, space in types
    let query = query.replace("signed integer", "bigint");
    let query = query.replace("SIGNED INTEGER", "bigint");
    let query = query.replace("unsigned integer", "bigint");
    let query = query.replace("UNSIGNED INTEGER", "bigint");

    // DBEver
    let query = query.replace(
        "SELECT db.oid,db.* FROM pg_catalog.pg_database db",
        "SELECT db.oid as _oid,db.* FROM pg_catalog.pg_database db",
    );
    let query = query.replace(
        "SELECT t.oid,t.*,c.relkind",
        "SELECT t.oid as _oid,t.*,c.relkind",
    );
    let query = query.replace(
        "SELECT n.oid,n.*,d.description FROM",
        "SELECT n.oid as _oid,n.*,d.description FROM",
    );

    // TODO support these introspection Superset queries
    let query = query.replace(
        "(SELECT pg_catalog.pg_get_expr(d.adbin, d.adrelid)\
\n                FROM pg_catalog.pg_attrdef d\
\n               WHERE d.adrelid = a.attrelid AND d.adnum = a.attnum\
\n               AND a.atthasdef)\
\n              AS DEFAULT",
        "NULL AS DEFAULT",
    );

    let query = query.replace(
        "SELECT\
\n                  i.relname as relname,\
\n                  ix.indisunique, ix.indexprs, ix.indpred,\
\n                  a.attname, a.attnum, c.conrelid, ix.indkey::varchar,\
\n                  ix.indoption::varchar, i.reloptions, am.amname,\
\n                  ix.indnkeyatts as indnkeyatts\
\n              FROM\
\n                  pg_class t\
\n                        join pg_index ix on t.oid = ix.indrelid\
\n                        join pg_class i on i.oid = ix.indexrelid\
\n                        left outer join\
\n                            pg_attribute a\
\n                            on t.oid = a.attrelid and a.attnum = ANY(ix.indkey)\
\n                        left outer join\
\n                            pg_constraint c\
\n                            on (ix.indrelid = c.conrelid and\
\n                                ix.indexrelid = c.conindid and\
\n                                c.contype in ('p', 'u', 'x'))\
\n                        left outer join\
\n                            pg_am am\
\n                            on i.relam = am.oid\
\n              WHERE\
\n                  t.relkind IN ('r', 'v', 'f', 'm', 'p')",
        "SELECT\
\n                  i.relname as relname,\
\n                  ix.indisunique, ix.indexprs, ix.indpred,\
\n                  a.attname, a.attnum, c.conrelid, ix.indkey,\
\n                  ix.indoption, i.reloptions, am.amname,\
\n                  ix.indnkeyatts as indnkeyatts\
\n              FROM\
\n                  pg_class t\
\n                        join pg_index ix on t.oid = ix.indrelid\
\n                        join pg_class i on i.oid = ix.indexrelid\
\n                        left outer join\
\n                            pg_attribute a\
\n                            on t.oid = a.attrelid\
\n                        left outer join\
\n                            pg_constraint c\
\n                            on (ix.indrelid = c.conrelid and\
\n                                ix.indexrelid = c.conindid and\
\n                                c.contype in ('p', 'u', 'x'))\
\n                        left outer join\
\n                            pg_am am\
\n                            on i.relam = am.oid\
\n              WHERE\
\n                  t.relkind IN ('r', 'v', 'f', 'm', 'p')",
    );

    let query = query.replace(
        "and ix.indisprimary = 'f'\
\n              ORDER BY\
\n                  t.relname,\
\n                  i.relname",
        "and ix.indisprimary = false",
    );

    let query = query.replace(
        "on t.oid = a.attrelid and a.attnum = ANY(ix.indkey)",
        "on t.oid = a.attrelid",
    );

    // TODO: Quick workaround for Tableau Desktop (ODBC), waiting for DF rebase...
    // Right now, our fork of DF doesn't support ON conditions with this filter
    let query = query.replace(
        "left outer join pg_attrdef d on a.atthasdef and",
        "left outer join pg_attrdef d on",
    );

    let query = query.replace("a.attnum = ANY(cons.conkey)", "1 = 1");
    let query = query.replace("pg_get_constraintdef(cons.oid) as src", "NULL as src");

    // ThoughtSpot (Redshift)
    // Subquery must have alias, It's a default Postgres behaviour, but Redshift is based on top of old Postgres version...
    let query = query.replace(
        // Subquery must have alias
        "AS REF_GENERATION  FROM svv_tables) WHERE true  AND current_database() = ",
        "AS REF_GENERATION  FROM svv_tables) as svv_tables WHERE current_database() =",
    );
    let query = query.replace("AND TABLE_TYPE IN ( 'TABLE', 'VIEW', 'EXTERNAL TABLE')", "");
    let query = query.replace(
        // Subquery must have alias
        // Incorrect alias for subquery
        "FROM (select lbv_cols.schemaname, lbv_cols.tablename, lbv_cols.columnname,REGEXP_REPLACE(REGEXP_REPLACE(lbv_cols.columntype,'\\\\(.*\\\\)'),'^_.+','ARRAY') as columntype_rep,columntype, lbv_cols.columnnum from pg_get_late_binding_view_cols() lbv_cols( schemaname name, tablename name, columnname name, columntype text, columnnum int)) lbv_columns   WHERE",
        "FROM (select schemaname, tablename, columnname,REGEXP_REPLACE(REGEXP_REPLACE(columntype,'\\\\(.*\\\\)'),'^_.+','ARRAY') as columntype_rep,columntype, columnnum from get_late_binding_view_cols_unpacked) as lbv_columns   WHERE",
    );
    let query = query.replace(
        // Subquery must have alias
        "ORDER BY TABLE_SCHEM,c.relname,attnum )  UNION ALL SELECT current_database()::VARCHAR(128) AS TABLE_CAT",
        "ORDER BY TABLE_SCHEM,c.relname,attnum ) as t  UNION ALL SELECT current_database()::VARCHAR(128) AS TABLE_CAT",
    );
    let query = query.replace(
        // Reusage of new column in another column
        "END AS IS_AUTOINCREMENT, IS_AUTOINCREMENT AS IS_GENERATEDCOLUMN",
        "END AS IS_AUTOINCREMENT, false AS IS_GENERATEDCOLUMN",
    );

    // Sigma Computing WITH query workaround
    let query = if SIGMA_WORKAROUND.is_match(&query) {
        let relnamespace_re = Regex::new(r#"(?s)from\spg_catalog\.pg_class\s+where\s+relname\s=\s(?P<relname>'(?:[^']|'')+'|\$\d+)\s+and\s+relnamespace\s=\s\(select\soid\sfrom\snsp\)"#).unwrap();
        let relnamespace_replaced = relnamespace_re.replace(
            &query,
            "from pg_catalog.pg_class join nsp on relnamespace = nsp.oid where relname = $relname",
        );
        let attrelid_re = Regex::new(r#"(?s)left\sjoin\spg_description\son\s+attrelid\s=\sobjoid\sand\s+attnum\s=\sobjsubid\s+where\s+attnum\s>\s0\s+and\s+attrelid\s=\s\(select\soid\sfrom\stbl\)"#).unwrap();
        let attrelid_replaced = attrelid_re.replace(&relnamespace_replaced, "left join pg_description on attrelid = objoid and attnum = objsubid join tbl on attrelid = tbl.oid where attnum > 0");
        attrelid_replaced.to_string()
    } else {
        query
    };

    // Metabase
    // TODO: To Support InSubquery Node (waiting for rebase DF)
    let query = query.replace(
        "WHERE t.oid IN (SELECT DISTINCT enumtypid FROM pg_enum e)",
        "WHERE t.oid = 0",
    );

    // Holistics.io
    // TODO: Waiting for rebase DF
    // Right now, our fork of DF doesn't support ON conditions with this filter
    let query = query.replace(
        "ON c.conrelid=ta.attrelid AND ta.attnum=c.conkey[o.ord]",
        "ON c.conrelid=ta.attrelid",
    );

    // Holistics.io
    // TODO: Waiting for rebase DF
    // Right now, our fork of DF doesn't support ON conditions with this filter
    let query = query.replace(
        "ON c.confrelid=fa.attrelid AND fa.attnum=c.confkey[o.ord]",
        "ON c.confrelid=fa.attrelid",
    );

    // Holistics.io
    // TODO: To Support InSubquery Node (waiting for rebase DF)
    let query = query.replace(
        "AND c.relname IN (SELECT table_name\nFROM information_schema.tables\nWHERE (table_type = 'BASE TABLE' OR table_type = 'VIEW')\n  AND table_schema NOT IN ('pg_catalog', 'information_schema')\n  AND has_schema_privilege(table_schema, 'USAGE'::text)\n)\n",
        "",
    );

    // Microstrategy
    // TODO: Support Subquery Node
    let query = query.replace("= (SELECT current_schema())", "= current_schema()");

    // Grafana
    // TODO: Support InSubquery Node
    let query = query.replace(
        "WHERE quote_ident(table_schema) NOT IN ('information_schema', 'pg_catalog', '_timescaledb_cache', '_timescaledb_catalog', '_timescaledb_internal', '_timescaledb_config', 'timescaledb_information', 'timescaledb_experimental') AND table_type = 'BASE TABLE' AND quote_ident(table_schema) IN (SELECT CASE WHEN TRIM(s[i]) = '\"$user\"' THEN user ELSE TRIM(s[i]) END FROM generate_series(array_lower(string_to_array(current_setting('search_path'), ','), 1), array_upper(string_to_array(current_setting('search_path'), ','), 1)) AS i, string_to_array(current_setting('search_path'), ',') AS s)",
        "WHERE quote_ident(table_schema) IN (current_user, current_schema()) AND table_type = 'BASE TABLE'"
    );
    let query = query.replace(
        "where quote_ident(table_schema) not in ('information_schema',\
\n                             'pg_catalog',\
\n                             '_timescaledb_cache',\
\n                             '_timescaledb_catalog',\
\n                             '_timescaledb_internal',\
\n                             '_timescaledb_config',\
\n                             'timescaledb_information',\
\n                             'timescaledb_experimental')\
\n      and \
\n          quote_ident(table_schema) IN (\
\n          SELECT\
\n            CASE WHEN trim(s[i]) = '\"$user\"' THEN user ELSE trim(s[i]) END\
\n          FROM\
\n            generate_series(\
\n              array_lower(string_to_array(current_setting('search_path'),','),1),\
\n              array_upper(string_to_array(current_setting('search_path'),','),1)\
\n            ) as i,\
\n            string_to_array(current_setting('search_path'),',') s\
\n          )",
        "WHERE quote_ident(table_schema) IN (current_user, current_schema())",
    );

    // psqlODBC
    let query = query.replace(
        "select NULL, NULL, NULL",
        "select NULL, NULL AS NULL2, NULL AS NULL3",
    );

    if let Some(qtrace) = qtrace {
        qtrace.set_replaced_query(&query)
    }

    let parse_result = match protocol {
        DatabaseProtocol::MySQL => Parser::parse_sql(&MySqlDialectWithBackTicks {}, query.as_str()),
        DatabaseProtocol::PostgreSQL => Parser::parse_sql(&PostgreSqlDialect {}, query.as_str()),
    };

    parse_result.map_err(|err| {
        CompilationError::user(format!("Unable to parse: {:?}", err))
            .with_meta(Some(HashMap::from([("query".to_string(), original_query)])))
    })
}

pub fn parse_sql_to_statement(
    query: &String,
    protocol: DatabaseProtocol,
    qtrace: &mut Option<Qtrace>,
) -> CompilationResult<Statement> {
    match parse_sql_to_statements(query, protocol, qtrace)? {
        stmts => {
            if stmts.len() == 1 {
                Ok(stmts[0].clone())
            } else {
                let err = if stmts.is_empty() {
                    CompilationError::user(format!(
                        "Invalid query, no statements was specified: {}",
                        &query
                    ))
                } else {
                    CompilationError::unsupported(format!(
                        "Multiple statements was specified in one query: {}",
                        &query
                    ))
                };

                Err(err.with_meta(Some(HashMap::from([("query".to_string(), query.clone())]))))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_statements_mysql() {
        let result = parse_sql_to_statement(
            &"-- 6dcd92a04feb50f14bbcf07c661680ba SELECT NOW".to_string(),
            DatabaseProtocol::MySQL,
            &mut None,
        );
        match result {
            Ok(_) => panic!("This test should throw an error"),
            Err(err) => assert_eq!(
                true,
                err.to_string()
                    .contains("Invalid query, no statements was specified")
            ),
        }
    }

    #[test]
    fn test_multiple_statements_mysql() {
        let result = parse_sql_to_statement(
            &"SELECT NOW(); SELECT NOW();".to_string(),
            DatabaseProtocol::MySQL,
            &mut None,
        );
        match result {
            Ok(_) => panic!("This test should throw an error"),
            Err(err) => assert_eq!(
                true,
                err.to_string()
                    .contains("Multiple statements was specified in one query")
            ),
        }
    }

    #[test]
    fn test_single_line_comments_mysql() {
        let result = parse_sql_to_statement(
            &"-- 6dcd92a04feb50f14bbcf07c661680ba
            SELECT DATE(`createdAt`) AS __timestamp,
                   COUNT(*) AS count
            FROM db.`Orders`
            GROUP BY DATE(`createdAt`)
            ORDER BY count DESC
            LIMIT 10000
            -- 6dcd92a04feb50f14bbcf07c661680ba
        "
            .to_string(),
            DatabaseProtocol::MySQL,
            &mut None,
        );
        match result {
            Ok(_) => {}
            Err(err) => panic!("{}", err),
        }
    }

    #[test]
    fn test_no_statements_postgres() {
        let result = parse_sql_to_statement(
            &"-- 6dcd92a04feb50f14bbcf07c661680ba SELECT NOW".to_string(),
            DatabaseProtocol::PostgreSQL,
            &mut None,
        );
        match result {
            Ok(_) => panic!("This test should throw an error"),
            Err(err) => assert_eq!(
                true,
                err.to_string()
                    .contains("Invalid query, no statements was specified")
            ),
        }
    }

    #[test]
    fn test_multiple_statements_postgres() {
        let result = parse_sql_to_statement(
            &"SELECT NOW(); SELECT NOW();".to_string(),
            DatabaseProtocol::PostgreSQL,
            &mut None,
        );
        match result {
            Ok(_) => panic!("This test should throw an error"),
            Err(err) => assert_eq!(
                true,
                err.to_string()
                    .contains("Multiple statements was specified in one query")
            ),
        }
    }

    #[test]
    fn test_single_line_comments_postgres() {
        let result = parse_sql_to_statement(
            &"-- 6dcd92a04feb50f14bbcf07c661680ba
            SELECT createdAt AS __timestamp,
                   COUNT(*) AS count
            FROM Orders
            GROUP BY createdAt
            ORDER BY count DESC
            LIMIT 10000
            -- 6dcd92a04feb50f14bbcf07c661680ba
        "
            .to_string(),
            DatabaseProtocol::PostgreSQL,
            &mut None,
        );
        match result {
            Ok(_) => {}
            Err(err) => panic!("{}", err),
        }
    }
}
