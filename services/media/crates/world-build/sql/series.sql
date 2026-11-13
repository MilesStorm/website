-- Stage series (02-world-data.md §5). Table series_overrides (work, name, position) is filled
-- from series-overrides.toml before this runs.

-- 1. Goodreads: the book's first series id; the position from "(Name, #3)" at the end of its
--    title. Majority over the books of the work's Goodreads works.
CREATE OR REPLACE TABLE series_gr AS
WITH books AS (
    SELECT m.ol_work AS work, b.series[1] AS series_id,
           TRY_CAST(regexp_extract(b.title, '#([0-9]+(?:\.[0-9]+)?)\)\s*$', 1) AS DOUBLE) AS position,
           b.ratings_count
    FROM gr_books b JOIN gr_work_map m ON m.gr_work = b.work_id
    WHERE len(b.series) > 0
),
best AS (
    SELECT work, series_id
    FROM (SELECT work, series_id, row_number() OVER (PARTITION BY work ORDER BY count(*) DESC, sum(ratings_count) DESC, series_id) AS rank
          FROM books GROUP BY work, series_id)
    WHERE rank = 1
)
SELECT b.work, b.series_id, s.title AS name, series_norm(s.title) AS norm,
       (SELECT mode(x.position) FROM books x WHERE x.work = b.work AND x.series_id = b.series_id AND x.position IS NOT NULL) AS position
FROM best b JOIN gr_series s ON s.series_id = b.series_id
WHERE s.title IS NOT NULL;

-- 2. Open Library edition strings, parsed. Per work the most common (name, number), over its
--    English editions when it has any, else over all.
CREATE OR REPLACE TABLE series_ol AS
WITH parsed AS (
    SELECT e.work, list_contains(e.languages, 'eng') AS english, series_parse(s) AS p
    FROM ol_editions e, unnest(e.series) AS t(s)
    WHERE e.work IS NOT NULL
),
named AS (
    SELECT work, english, p.name AS name, series_norm(p.name) AS norm, p.number AS number
    FROM parsed WHERE series_norm(p.name) <> ''
),
-- A work with English editions counts only those: other languages' strings are mostly
-- publishers' collections ("Klub čtenářů -- svazek 639").
english_works AS (
    SELECT DISTINCT work FROM ol_editions WHERE list_contains(languages, 'eng')
),
scoped AS (
    SELECT * FROM named n
    WHERE english OR n.work NOT IN (SELECT work FROM english_works)
)
SELECT work, norm, name, number
FROM (SELECT work, norm, number, mode(name) AS name,
             row_number() OVER (PARTITION BY work ORDER BY count(*) DESC, number IS NULL, norm) AS rank
      FROM scoped GROUP BY work, norm, number)
WHERE rank = 1;

-- Authors of each Goodreads series' works, to tell series with the same name apart.
CREATE OR REPLACE TABLE series_gr_authors AS
SELECT DISTINCT g.series_id, g.norm, unnest(w.authors) AS author
FROM series_gr g JOIN ol_works w ON w.work = g.work;

-- 3. Merge: an Open Library series is the Goodreads one when the normalised names are equal and
--    they share an author; otherwise it is its own series (keyed by name and first author).
CREATE OR REPLACE TABLE work_series AS
WITH ol_matched AS (
    SELECT o.work, o.name, o.number,
           (SELECT min(a.series_id) FROM series_gr_authors a
            WHERE a.norm = o.norm AND list_contains(w.authors, a.author)) AS series_id,
           o.norm, w.authors[1] AS first_author
    FROM series_ol o JOIN ol_works w ON w.work = o.work
),
merged AS (
    SELECT g.work, 'gr:' || g.series_id AS series_key, g.name,
           coalesce(g.position, (SELECT o.number FROM series_ol o WHERE o.work = g.work AND o.norm = g.norm)) AS position
    FROM series_gr g
    UNION ALL
    SELECT o.work,
           CASE WHEN o.series_id IS NOT NULL THEN 'gr:' || o.series_id ELSE 'ol:' || o.norm || ':' || coalesce(o.first_author, 0) END,
           coalesce((SELECT g.title FROM gr_series g WHERE g.series_id = o.series_id), o.name),
           o.number
    FROM ol_matched o
    WHERE o.work NOT IN (SELECT work FROM series_gr)
)
-- 4. Overrides win.
SELECT m.work, m.series_key, m.name, m.position FROM merged m
WHERE m.work NOT IN (SELECT work FROM series_overrides)
UNION ALL
SELECT work, 'override:' || series_norm(name), name, position FROM series_overrides;
