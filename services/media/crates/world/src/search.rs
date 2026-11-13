//! The search index: one document per searchable work. Case- and accent-insensitive; every
//! word must match and the last one matches as a prefix (the user is still typing). With
//! fewer than `retry_below` hits the query is retried with every word as a prefix and one
//! typo allowed in longer words. Hits rank by text relevance plus a popularity bonus.

use serde::{Deserialize, Serialize};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, BoostQuery, FuzzyTermQuery, Occur, Query, TermQuery};
use tantivy::schema::{
    FAST, Field, INDEXED, IndexRecordOption, STORED, Schema, TextFieldIndexing, TextOptions,
    Value as _,
};
use tantivy::tokenizer::{
    AsciiFoldingFilter, LowerCaser, RemoveLongFilter, SimpleTokenizer, TextAnalyzer,
};
use tantivy::{DocAddress, Index, IndexReader, Score, SegmentReader, TantivyDocument, Term};

use crate::{Error, Result, SearchParams, WorkKey};

/// The tokenizer of every text field and of queries.
pub(crate) const TOKENIZER: &str = "fold";

pub(crate) fn analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(40))
        .filter(LowerCaser)
        .filter(AsciiFoldingFilter)
        .build()
}

/// Apostrophes join their word instead of splitting it: "Hitchhiker's" is found by
/// "hitchhikers" (and by "hitchhiker" as a prefix).
pub(crate) fn normalize(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '\'' | '\u{2019}' | '\u{2018}' | '`'))
        .collect()
}

/// The schema's fields.
#[derive(Clone, Copy)]
pub(crate) struct Fields {
    pub key: Field,
    /// Searched (normalised, not stored); the `*_shown` fields keep the text as written.
    pub title: Field,
    pub title_shown: Field,
    pub subtitle: Field,
    pub other_titles: Field,
    pub authors: Field,
    pub authors_shown: Field,
    pub series: Field,
    pub series_shown: Field,
    pub series_position: Field,
    pub year: Field,
    pub cover: Field,
    pub popularity: Field,
    /// 1 for summaries, study guides, box sets: ranked below the books themselves.
    pub not_a_book: Field,
}

pub(crate) fn schema() -> (Schema, Fields) {
    let text = |stored: bool| {
        let options = TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(TOKENIZER)
                .set_index_option(IndexRecordOption::WithFreqs),
        );
        if stored {
            options.set_stored()
        } else {
            options
        }
    };
    let mut b = Schema::builder();
    let fields = Fields {
        key: b.add_u64_field("key", INDEXED | STORED | FAST),
        title: b.add_text_field("title", text(false)),
        title_shown: b.add_text_field("title_shown", STORED),
        subtitle: b.add_text_field("subtitle", STORED),
        other_titles: b.add_text_field("other_titles", text(true)),
        authors: b.add_text_field("authors", text(false)),
        authors_shown: b.add_text_field("authors_shown", STORED),
        series: b.add_text_field("series", text(false)),
        series_shown: b.add_text_field("series_shown", STORED),
        series_position: b.add_f64_field("series_position", STORED),
        year: b.add_i64_field("year", STORED),
        cover: b.add_u64_field("cover", STORED),
        popularity: b.add_f64_field("popularity", FAST | STORED),
        not_a_book: b.add_u64_field("not_a_book", FAST),
    };
    (b.build(), fields)
}

pub(crate) fn register_tokenizer(index: &Index) {
    index.tokenizers().register(TOKENIZER, analyzer());
}

/// One search result: everything a result row shows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    pub key: WorkKey,
    pub title: String,
    pub subtitle: Option<String>,
    pub authors: Vec<String>,
    pub series: Option<String>,
    pub series_position: Option<f32>,
    pub year: Option<i16>,
    pub cover: Option<u64>,
    pub score: f32,
}

pub(crate) fn hit(fields: &Fields, doc: &TantivyDocument, score: f32) -> Result<Hit> {
    let text = |f: Field| doc.get_first(f).and_then(|v| v.as_str()).map(str::to_owned);
    let key = doc
        .get_first(fields.key)
        .and_then(|v| v.as_u64())
        .ok_or_else(|| Error::Corrupt {
            path: crate::files::SEARCH_DIR.into(),
            what: "a document without a key".into(),
        })?;
    Ok(Hit {
        key: WorkKey(key),
        title: text(fields.title_shown).unwrap_or_default(),
        subtitle: text(fields.subtitle),
        authors: doc
            .get_all(fields.authors_shown)
            .filter_map(|v| v.as_str())
            .map(str::to_owned)
            .collect(),
        series: text(fields.series_shown),
        series_position: doc
            .get_first(fields.series_position)
            .and_then(|v| v.as_f64())
            .map(|p| p as f32),
        year: doc
            .get_first(fields.year)
            .and_then(|v| v.as_i64())
            .and_then(|y| i16::try_from(y).ok()),
        cover: doc.get_first(fields.cover).and_then(|v| v.as_u64()),
        score,
    })
}

/// Text as search compares it: the index's words joined by spaces ("The Hitchhiker's Guide" →
/// "the hitchhikers guide").
pub fn fold(text: &str) -> String {
    tokens(text).join(" ")
}

/// The words of a query, as the index stores them.
pub(crate) fn tokens(text: &str) -> Vec<String> {
    let mut analyzer = analyzer();
    let text = normalize(text);
    let mut stream = analyzer.token_stream(&text);
    let mut out = Vec::new();
    while stream.advance() {
        out.push(stream.token().text.clone());
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pass {
    /// Every word exact except the last, which is a prefix.
    Strict,
    /// Every word a prefix; one typo in words of `typo_min_len` letters or more.
    Loose,
}

fn query(fields: &Fields, params: &SearchParams, words: &[String], pass: Pass) -> BooleanQuery {
    let targets = [
        (fields.title, params.title_boost),
        (fields.other_titles, params.other_titles_boost),
        (fields.authors, params.authors_boost),
        (fields.series, params.series_boost),
    ];
    let last = words.len() - 1;
    let per_word = words.iter().enumerate().map(|(i, word)| {
        let prefix = pass == Pass::Loose || i == last;
        let typo = pass == Pass::Loose && word.chars().count() >= params.typo_min_len;
        let distance = u8::from(typo);
        let alternatives: Vec<(Occur, Box<dyn Query>)> = targets
            .iter()
            .flat_map(|&(field, boost)| {
                let term = Term::from_field_text(field, word);
                // The exact word gives the BM25 relevance; a prefix or typo match only lets the
                // work in (fuzzy queries score a constant).
                let exact: Box<dyn Query> =
                    Box::new(TermQuery::new(term.clone(), IndexRecordOption::WithFreqs));
                let mut alts = vec![(
                    Occur::Should,
                    Box::new(BoostQuery::new(exact, boost)) as Box<dyn Query>,
                )];
                if prefix || typo {
                    let fuzzy = if prefix {
                        FuzzyTermQuery::new_prefix(term, distance, true)
                    } else {
                        FuzzyTermQuery::new(term, distance, true)
                    };
                    alts.push((
                        Occur::Should,
                        Box::new(BoostQuery::new(Box::new(fuzzy), boost * 0.5)),
                    ));
                }
                alts
            })
            .collect();
        (
            Occur::Must,
            Box::new(BooleanQuery::new(alternatives)) as Box<dyn Query>,
        )
    });
    BooleanQuery::new(per_word.collect())
}

pub(crate) fn search(
    index: &IndexReader,
    fields: &Fields,
    params: &SearchParams,
    text: &str,
    limit: usize,
) -> Result<Vec<Hit>> {
    let words = tokens(text);
    if words.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let searcher = index.searcher();
    let (weight, penalty) = (params.popularity_weight, params.not_a_book_penalty);
    // More candidates than asked for: the whole-title bonus below may reorder them.
    let candidates = (limit * 4).max(40);
    let run = |pass: Pass| -> Result<Vec<(Score, DocAddress)>> {
        let q = query(fields, params, &words, pass);
        let collector =
            TopDocs::with_limit(candidates).tweak_score(move |segment: &SegmentReader| {
                let popularity = segment
                    .fast_fields()
                    .f64("popularity")
                    .map(|c| c.first_or_default_col(0.0))
                    .expect("the schema has a fast popularity field");
                let not_a_book = segment
                    .fast_fields()
                    .u64("not_a_book")
                    .map(|c| c.first_or_default_col(0))
                    .expect("the schema has a fast not_a_book field");
                move |doc, score: Score| {
                    score + weight * popularity.get_val(doc) as Score
                        - penalty * not_a_book.get_val(doc) as Score
                }
            });
        Ok(searcher.search(&q, &collector)?)
    };
    let mut found = run(Pass::Strict)?;
    if found.len() < params.retry_below.min(limit) {
        found = run(Pass::Loose)?;
    }
    let mut hits = found
        .into_iter()
        .map(|(score, address)| {
            let doc: TantivyDocument = searcher.doc(address)?;
            let whole = doc
                .get_all(fields.title_shown)
                .chain(doc.get_all(fields.other_titles))
                .filter_map(|v| v.as_str())
                .any(|title| is_whole_title(&words, title));
            let bonus = if whole { params.whole_title_bonus } else { 0.0 };
            hit(fields, &doc, score + bonus)
        })
        .collect::<Result<Vec<Hit>>>()?;
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    hits.truncate(limit);
    Ok(hits)
}

/// The query is the whole title, the last word possibly unfinished: "pride and prej" is
/// "Pride and Prejudice", not "Pride and Prejudice and Zombies".
fn is_whole_title(words: &[String], title: &str) -> bool {
    let title = tokens(title);
    title.len() == words.len()
        && words.iter().zip(&title).enumerate().all(|(i, (w, t))| {
            if i + 1 == words.len() {
                t.starts_with(w.as_str())
            } else {
                t == w
            }
        })
}

pub(crate) fn by_key(index: &IndexReader, fields: &Fields, key: WorkKey) -> Result<Option<Hit>> {
    let searcher = index.searcher();
    let q = TermQuery::new(
        Term::from_field_u64(fields.key, key.0),
        IndexRecordOption::Basic,
    );
    let found = searcher.search(&q, &TopDocs::with_limit(1).order_by_score())?;
    match found.first() {
        Some(&(_, address)) => Ok(Some(hit(fields, &searcher.doc(address)?, 0.0)?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_case_accents_and_apostrophes() {
        assert_eq!(
            tokens("The Hitchhiker's GUIDE"),
            ["the", "hitchhikers", "guide"]
        );
        assert_eq!(tokens("García Márquez"), ["garcia", "marquez"]);
        assert_eq!(
            tokens("Nineteen Eighty-Four"),
            ["nineteen", "eighty", "four"]
        );
        assert_eq!(tokens("Dirk Gently’s"), ["dirk", "gentlys"]);
        assert!(tokens("  ,. ").is_empty());
    }

    #[test]
    fn whole_titles() {
        let words = tokens("pride and prej");
        assert!(is_whole_title(&words, "Pride and Prejudice"));
        assert!(!is_whole_title(&words, "Pride and Prejudice and Zombies"));
        assert!(is_whole_title(&tokens("1984"), "1984"));
        assert!(!is_whole_title(&tokens("1984"), "SparkNotes for 1984"));
    }
}
