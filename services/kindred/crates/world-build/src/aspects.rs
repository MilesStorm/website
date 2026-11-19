//! Stage aspects: `aspects.toml` into the tables `sql/aspects.sql` matches against, and the
//! report of shelves no aspect matched (03-engine.md §5).

use crate::config::Aspects;
use crate::stages::Run;
use crate::trace::Duck;

/// aspect_def(k, grp), aspect_shelf(k, shelf), aspect_subject(k, phrase), org_shelf(shelf).
pub fn load(duck: &Duck, aspects: &Aspects) -> anyhow::Result<()> {
    aspects.check()?;
    duck.exec(
        "aspect_tables",
        "CREATE OR REPLACE TABLE aspect_def (k INTEGER, grp VARCHAR);
         CREATE OR REPLACE TABLE aspect_shelf (k INTEGER, shelf VARCHAR);
         CREATE OR REPLACE TABLE aspect_subject (k INTEGER, phrase VARCHAR);
         CREATE OR REPLACE TABLE org_shelf (shelf VARCHAR);",
    )?;
    let conn = duck.conn();
    let mut def = conn.appender("aspect_def")?;
    let mut shelf = conn.appender("aspect_shelf")?;
    let mut subject = conn.appender("aspect_subject")?;
    for (k, a) in aspects.aspect.iter().enumerate() {
        let k = k as i32;
        def.append_row(duckdb::params![k, a.group])?;
        for s in &a.shelves {
            shelf.append_row(duckdb::params![k, s.to_lowercase()])?;
        }
        for p in &a.subjects {
            // A phrase is matched as words inside a regular expression: escape it.
            subject.append_row(duckdb::params![k, regex_escape(&p.to_lowercase())])?;
        }
    }
    def.flush()?;
    shelf.flush()?;
    subject.flush()?;
    let mut org = conn.appender("org_shelf")?;
    for s in &aspects.organisational.shelves {
        org.append_row(duckdb::params![s.to_lowercase()])?;
    }
    org.flush()?;
    Ok(())
}

fn regex_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if "\\.+*?()|[]{}^$".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Writes the most-used shelves that no aspect and no organisational rule matched to
/// `model/unmatched-shelves.txt` on the scratch volume, so aspects.toml can be extended with
/// evidence. Not to the logs: shelf names are often titles and series ("harry-potter"), and logs
/// carry no titles (PLAN.md rule 14). The log line has the counts only.
pub fn report_unmatched(run: &Run) -> anyhow::Result<()> {
    let mut statement = run
        .duck
        .conn()
        .prepare("SELECT shelf, n FROM unmatched_shelves ORDER BY n DESC, shelf")?;
    let rows = statement
        .query_map([], |r| Ok(format!("{}:{}", r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    let (works, with_aspects): (i64, i64) = run.duck.conn().query_row(
        "SELECT (SELECT count(*) FROM aspect_totals), (SELECT count(DISTINCT work) FROM work_aspects)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    std::fs::write(run.model.join("unmatched-shelves.txt"), rows.join("\n"))?;
    tracing::info!(
        works,
        with_aspects,
        unmatched = rows.len(),
        "aspects matched; unmatched shelves in model/unmatched-shelves.txt"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_phrases_are_escaped() {
        assert_eq!(regex_escape("children's (fiction)"), "children's \\(fiction\\)");
        assert_eq!(regex_escape("sci-fi"), "sci-fi");
    }
}
