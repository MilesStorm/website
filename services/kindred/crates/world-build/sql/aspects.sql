-- Stage aspects (03-engine.md §2 "Aspect block", §5): Goodreads shelf tags and Open Library
-- subjects matched against aspects.toml, one number per (work, aspect).
-- Tables loaded by kindred-world from aspects.toml before this runs: aspect_def(k, grp),
-- aspect_shelf(k, shelf), aspect_subject(k, phrase), org_shelf(shelf).
-- Variables: gr_books (the downloaded file), org_patterns (one regular expression, whole name),
-- aspect_alpha, subject_weight, min_shelvings, report_unmatched.

-- Shelvings per Open Library work. Goodreads lists a work's popular shelves on each of its books
-- (editions), the same numbers repeated: take each Goodreads work's shelf once (the largest
-- count), then add up the Goodreads works mapped to the Open Library work. The counts are
-- strings in the file.
CREATE OR REPLACE TABLE work_shelves AS
WITH per_gr_work AS (
    SELECT gr_work, lower(trim(s.name)) AS shelf, max(TRY_CAST(s.count AS BIGINT)) AS n
    FROM (
        SELECT TRY_CAST(work_id AS BIGINT) AS gr_work, unnest(popular_shelves) AS s
        FROM read_json(getvariable('gr_books'), format = 'newline_delimited', compression = 'gzip',
                       columns = {'work_id': 'VARCHAR', 'popular_shelves': 'STRUCT(count VARCHAR, name VARCHAR)[]'})
    )
    WHERE TRY_CAST(s.count AS BIGINT) > 0 AND s.name IS NOT NULL
    GROUP BY ALL
)
SELECT m.ol_work AS work, p.shelf, sum(p.n) AS n
FROM per_gr_work p
JOIN gr_work_map m ON m.gr_work = p.gr_work
GROUP BY ALL;

-- Where or when, not what: to-read, owned, read-in-2016, 5-stars, ...
CREATE OR REPLACE TABLE shelf_kind AS
SELECT shelf, shelf IN (SELECT shelf FROM org_shelf) OR regexp_full_match(shelf, getvariable('org_patterns')) AS organisational
FROM (SELECT DISTINCT shelf FROM work_shelves);

-- Subjects that match an aspect's phrases, as whole words. Matched once per distinct subject.
CREATE OR REPLACE TABLE subject_aspects AS
WITH subjects AS (SELECT DISTINCT lower(unnest(subjects)) AS subject FROM catalogue)
SELECT s.subject, p.k
FROM subjects s
JOIN aspect_subject p ON regexp_matches(s.subject, '\b' || p.phrase || '\b')
GROUP BY ALL;

-- Per work: shelvings without organisational shelves (plain), with them (all), and its subjects
-- each counting subject_weight. Only catalogue works: the others are never shown.
CREATE OR REPLACE TABLE aspect_totals AS
WITH shelved AS (
    SELECT w.work,
           sum(w.n) FILTER (WHERE NOT k.organisational) AS plain,
           sum(w.n) AS everything
    FROM work_shelves w JOIN shelf_kind k ON k.shelf = w.shelf
    GROUP BY w.work
)
SELECT c.work,
       coalesce(s.plain, 0) + getvariable('subject_weight') * len(c.subjects) AS n_plain,
       coalesce(s.everything, 0) + getvariable('subject_weight') * len(c.subjects) AS n_all
FROM catalogue c
LEFT JOIN shelved s ON s.work = c.work
WHERE coalesce(s.everything, 0) + len(c.subjects) > 0;

-- n_ik: shelvings on the aspect's shelves plus subject_weight per matching subject.
CREATE OR REPLACE TABLE aspect_counts AS
SELECT work, k, sum(n) AS n
FROM (
    SELECT w.work, a.k, w.n::DOUBLE AS n
    FROM work_shelves w JOIN aspect_shelf a ON a.shelf = w.shelf
    UNION ALL
    SELECT c.work, sa.k, getvariable('subject_weight') AS n
    FROM (SELECT work, lower(unnest(subjects)) AS subject FROM catalogue) c
    JOIN subject_aspects sa ON sa.subject = c.subject
)
WHERE work IN (SELECT work FROM aspect_totals)
GROUP BY work, k;

-- The aspect's global share p_k. Group reception counts against all shelvings.
CREATE OR REPLACE TABLE aspect_share AS
SELECT d.k, d.grp,
       coalesce((SELECT sum(n) FROM aspect_counts c WHERE c.k = d.k), 0)
         / (SELECT sum(CASE WHEN d.grp = 'reception' THEN n_all ELSE n_plain END) FROM aspect_totals) AS p
FROM aspect_def d;

-- raw = ln((n_ik + alpha p_k) / (N_i + alpha)) - ln p_k, for works with N_i >= min_shelvings
-- (the others are placed by their text). An aspect nobody used (p_k = 0) is 0 everywhere.
CREATE OR REPLACE TABLE aspect_raw AS
SELECT t.work, s.k,
       CASE WHEN s.p > 0 THEN
           ln((coalesce(c.n, 0) + getvariable('aspect_alpha') * s.p)
              / (CASE WHEN s.grp = 'reception' THEN t.n_all ELSE t.n_plain END + getvariable('aspect_alpha')))
           - ln(s.p)
       ELSE 0 END AS raw
FROM aspect_totals t
CROSS JOIN aspect_share s
LEFT JOIN aspect_counts c ON c.work = t.work AND c.k = s.k
WHERE t.n_plain >= getvariable('min_shelvings');

-- Standardised and clipped to +-3, one row per (work, aspect); kindred-world reads them in
-- (work, k) order. Long rather than one list per work: an ordered list is held in memory.
CREATE OR REPLACE TABLE work_aspects AS
WITH stats AS (SELECT k, avg(raw) AS mean, stddev_pop(raw) AS sd FROM aspect_raw GROUP BY k)
SELECT r.work, r.k,
       (CASE WHEN s.sd > 0 THEN greatest(-3, least(3, (r.raw - s.mean) / s.sd)) ELSE 0 END)::FLOAT AS a
FROM aspect_raw r JOIN stats s ON s.k = r.k;

DROP TABLE aspect_raw;

-- The most-used shelves that are neither organisational nor any aspect's (logged by the stage).
CREATE OR REPLACE TABLE unmatched_shelves AS
SELECT w.shelf, sum(w.n) AS n
FROM work_shelves w JOIN shelf_kind k ON k.shelf = w.shelf
WHERE NOT k.organisational AND w.shelf NOT IN (SELECT shelf FROM aspect_shelf)
GROUP BY w.shelf
ORDER BY n DESC, w.shelf
LIMIT getvariable('report_unmatched');
