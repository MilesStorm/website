-- Stage ingest: each input into a table with only the columns the later stages need.
-- Variables (set by kindred-world): the downloaded file of each input, ol_* and gr_*.

-- An Open Library dump: type, key, revision, last_modified, JSON; tab-separated, no header,
-- no quoting. Some records are long (descriptions, tables of contents).
CREATE OR REPLACE MACRO ol_dump(path, wanted) AS TABLE
    SELECT key, CAST(json AS JSON) AS j
    FROM read_csv(path, delim = '\t', quote = '', escape = '', header = false, auto_detect = false,
                  columns = {'type': 'VARCHAR', 'key': 'VARCHAR', 'revision': 'BIGINT', 'modified': 'VARCHAR', 'json': 'VARCHAR'},
                  max_line_size = 134217728, compression = 'gzip')
    WHERE type = wanted;

CREATE OR REPLACE TABLE ol_works AS
SELECT
    ol_num(key) AS work,
    nullif(trim(j ->> '$.title'), '') AS title,
    nullif(trim(j ->> '$.subtitle'), '') AS subtitle,
    -- A string, or {"type": "/type/text", "value": "..."}.
    nullif(trim(CASE WHEN json_type(j, '$.description') = 'OBJECT' THEN j ->> '$.description.value' ELSE j ->> '$.description' END), '') AS description,
    -- authors[].author.key; older records: authors[].author as a plain string.
    list_filter(
        list_transform(CAST(coalesce(j -> '$.authors', '[]') AS JSON[]),
                       lambda a: ol_num(coalesce(a ->> '$.author.key', a ->> '$.author'))),
        lambda x: x IS NOT NULL) AS authors,
    coalesce(CAST(j -> '$.subjects' AS VARCHAR[]), []) AS subjects,
    list_filter(coalesce(TRY_CAST(j -> '$.covers' AS BIGINT[]), []), lambda c: c >= 1) AS covers,
    year_of(j ->> '$.first_publish_date') AS first_publish_year
FROM ol_dump(getvariable('ol_works'), '/type/work')
WHERE ol_num(key) IS NOT NULL;

CREATE OR REPLACE TABLE ol_editions AS
SELECT
    ol_num(key) AS edition,
    ol_num(j ->> '$.works[0].key') AS work,
    nullif(trim(j ->> '$.title'), '') AS title,
    list_distinct(list_filter(
        list_transform(coalesce(CAST(j -> '$.isbn_13' AS VARCHAR[]), []) || coalesce(CAST(j -> '$.isbn_10' AS VARCHAR[]), []),
                       lambda written_isbn: isbn13(written_isbn)),
        lambda found_isbn: found_isbn IS NOT NULL)) AS isbns,
    list_filter(list_transform(coalesce(CAST(j -> '$.identifiers.goodreads' AS VARCHAR[]), []), lambda g: TRY_CAST(trim(g) AS BIGINT)),
                lambda g: g IS NOT NULL) AS goodreads,
    list_transform(coalesce(CAST(j -> '$.languages' AS JSON[]), []), lambda l: replace(l ->> '$.key', '/languages/', '')) AS languages,
    TRY_CAST(j ->> '$.number_of_pages' AS INTEGER) AS pages,
    list_filter(coalesce(TRY_CAST(j -> '$.series' AS VARCHAR[]), []), lambda s: trim(s) <> '') AS series,
    list_filter(coalesce(TRY_CAST(j -> '$.covers' AS BIGINT[]), []), lambda c: c >= 1) AS covers,
    year_of(j ->> '$.publish_date') AS publish_year
FROM ol_dump(getvariable('ol_editions'), '/type/edition')
WHERE ol_num(key) IS NOT NULL;

CREATE OR REPLACE TABLE ol_authors AS
SELECT ol_num(key) AS author, nullif(trim(j ->> '$.name'), '') AS name
FROM ol_dump(getvariable('ol_authors'), '/type/author')
WHERE ol_num(key) IS NOT NULL;

-- Merged works: old key -> the key it now points at, chains followed (at most 10 hops).
CREATE OR REPLACE TABLE ol_redirects AS
WITH RECURSIVE
    hop AS (
        SELECT ol_num(key) AS old, ol_num(j ->> '$.location') AS new
        FROM ol_dump(getvariable('ol_redirects'), '/type/redirect')
        WHERE key LIKE '/works/%' AND j ->> '$.location' LIKE '/works/%'
    ),
    chain(old, new, depth) AS (
        SELECT old, new, 1 FROM hop
        UNION ALL
        SELECT c.old, h.new, c.depth + 1 FROM chain c JOIN hop h ON h.old = c.new WHERE c.depth < 10
    )
SELECT old, arg_max(new, depth) AS new FROM chain WHERE old IS NOT NULL AND new IS NOT NULL GROUP BY old;

UPDATE ol_editions e SET work = r.new FROM ol_redirects r WHERE e.work = r.old;

-- ratings: work, edition or \N, rating 1-5, date. reading-log: work, edition or \N, shelf, date.
CREATE OR REPLACE TABLE ol_ratings AS
SELECT coalesce(r.new, x.work) AS work, x.rating
FROM (
    SELECT ol_num(work) AS work, TRY_CAST(rating AS INTEGER) AS rating
    FROM read_csv(getvariable('ol_ratings'), delim = '\t', quote = '', escape = '', header = false, auto_detect = false,
                  nullstr = '\N', columns = {'work': 'VARCHAR', 'edition': 'VARCHAR', 'rating': 'VARCHAR', 'date': 'VARCHAR'},
                  compression = 'gzip')
) x LEFT JOIN ol_redirects r ON r.old = x.work
WHERE x.work IS NOT NULL AND x.rating BETWEEN 1 AND 5;

CREATE OR REPLACE TABLE ol_reading_log AS
SELECT coalesce(r.new, x.work) AS work, x.shelf
FROM (
    SELECT ol_num(work) AS work, shelf
    FROM read_csv(getvariable('ol_reading_log'), delim = '\t', quote = '', escape = '', header = false, auto_detect = false,
                  nullstr = '\N', columns = {'work': 'VARCHAR', 'edition': 'VARCHAR', 'shelf': 'VARCHAR', 'date': 'VARCHAR'},
                  compression = 'gzip')
) x LEFT JOIN ol_redirects r ON r.old = x.work
WHERE x.work IS NOT NULL;

-- Goodreads: every value is a string, empty when missing. Field names are checked against the
-- file's first line before this runs (kindred-world, ingest).
CREATE OR REPLACE TABLE gr_books AS
SELECT
    TRY_CAST(book_id AS BIGINT) AS book_id,
    TRY_CAST(work_id AS BIGINT) AS work_id,
    coalesce(isbn13(nullif(isbn13, '')), isbn13(nullif(isbn, ''))) AS isbn13,
    coalesce(TRY_CAST(nullif(ratings_count, '') AS BIGINT), 0) AS ratings_count,
    list_filter(coalesce(series, []), lambda s: s <> '') AS series,
    title,
    title_without_series,
    nullif(language_code, '') AS language_code
FROM read_json(getvariable('gr_books'), format = 'newline_delimited', compression = 'gzip',
               columns = {'book_id': 'VARCHAR', 'work_id': 'VARCHAR', 'isbn': 'VARCHAR', 'isbn13': 'VARCHAR',
                          'ratings_count': 'VARCHAR', 'series': 'VARCHAR[]', 'title': 'VARCHAR',
                          'title_without_series': 'VARCHAR', 'language_code': 'VARCHAR'})
WHERE TRY_CAST(book_id AS BIGINT) IS NOT NULL;

CREATE OR REPLACE TABLE gr_works AS
SELECT
    TRY_CAST(work_id AS BIGINT) AS work_id,
    coalesce(TRY_CAST(nullif(ratings_count, '') AS BIGINT), 0) AS ratings_count,
    coalesce(TRY_CAST(nullif(ratings_sum, '') AS BIGINT), 0) AS ratings_sum,
    TRY_CAST(nullif(original_publication_year, '') AS SMALLINT) AS year
FROM read_json(getvariable('gr_works'), format = 'newline_delimited', compression = 'gzip',
               columns = {'work_id': 'VARCHAR', 'ratings_count': 'VARCHAR', 'ratings_sum': 'VARCHAR',
                          'original_publication_year': 'VARCHAR'})
WHERE TRY_CAST(work_id AS BIGINT) IS NOT NULL;

CREATE OR REPLACE TABLE gr_series AS
SELECT series_id, nullif(trim(title), '') AS title
FROM read_json(getvariable('gr_series'), format = 'newline_delimited', compression = 'gzip',
               columns = {'series_id': 'VARCHAR', 'title': 'VARCHAR'});
