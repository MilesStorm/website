-- Stage catalogue: which works a user can find (02-world-data.md §3).
-- Variables: min_goodreads_ratings, max_other_titles, min_description_chars, not_a_book_titles,
-- not_a_book_subjects (one case-insensitive regular expression each).

-- Edition titles that differ from the work's own, most editions first, then alphabetical: the same
-- world from the same inputs, and the titles most readers know are the ones kept. Cut to the kept
-- titles before the list is built: an ordered list stays in memory, and editions are too many.
CREATE OR REPLACE TABLE edition_titles AS
SELECT work, list(title ORDER BY editions DESC, title) AS titles
FROM (
    SELECT e.work, e.title, count(*) AS editions
    FROM ol_editions e
    JOIN ol_works w ON w.work = e.work
    WHERE e.title IS NOT NULL AND lower(e.title) <> lower(w.title)
    GROUP BY e.work, e.title
    QUALIFY row_number() OVER (PARTITION BY e.work ORDER BY count(*) DESC, e.title) <= getvariable('max_other_titles')
)
GROUP BY work;

-- Languages apart from the other facts: one list per work built from distinct values, instead of
-- every edition's list held until its work is complete.
CREATE OR REPLACE TABLE edition_facts AS
WITH work_languages AS (
    SELECT work, list_sort(list(language)) AS languages
    FROM (SELECT DISTINCT work, unnest(languages) AS language FROM ol_editions WHERE work IS NOT NULL)
    WHERE language IS NOT NULL
    GROUP BY work
),
facts AS (
    SELECT
        work,
        bool_or(len(isbns) > 0) AS has_isbn,
        median(pages) FILTER (WHERE pages BETWEEN 1 AND 20000) AS pages,
        min(publish_year) AS first_edition_year,
        arg_min(covers[1], edition) FILTER (WHERE len(covers) > 0) AS cover
    FROM ol_editions
    WHERE work IS NOT NULL
    GROUP BY work
)
SELECT f.work, f.has_isbn, f.pages, coalesce(l.languages, []) AS languages, f.first_edition_year, f.cover
FROM facts f
LEFT JOIN work_languages l ON l.work = f.work;

CREATE OR REPLACE TABLE reader_signs AS
SELECT work, sum(ol_log)::BIGINT AS ol_log, sum(ol_n)::BIGINT AS ol_n, sum(ol_sum)::BIGINT AS ol_sum
FROM (
    SELECT work, count(*) AS ol_log, 0 AS ol_n, 0 AS ol_sum FROM ol_reading_log GROUP BY work
    UNION ALL
    SELECT work, 0, count(*), sum(rating) FROM ol_ratings GROUP BY work
)
GROUP BY work;

-- Author names in the work's order.
CREATE OR REPLACE TABLE work_authors AS
SELECT a.work, list(n.name ORDER BY a.pos) AS authors
FROM (SELECT work, unnest(authors) AS author, generate_subscripts(authors, 1) AS pos FROM ol_works) a
JOIN ol_authors n ON n.author = a.author
WHERE n.name IS NOT NULL
GROUP BY a.work;

CREATE OR REPLACE TABLE catalogue AS
SELECT
    w.work,
    w.title,
    w.subtitle,
    wa.authors,
    coalesce(et.titles, []) AS other_titles,
    -- The work's own date when it has one; editions and Goodreads otherwise.
    coalesce(w.first_publish_year, least(e.first_edition_year, g.gr_year)) AS year,
    CAST(e.pages AS INTEGER) AS pages,
    e.languages,
    coalesce(w.covers[1], e.cover) AS cover,
    w.description,
    coalesce(length(w.description), 0) >= getvariable('min_description_chars') AS has_description,
    w.subjects,
    regexp_matches(w.title, getvariable('not_a_book_titles'), 'i')
        OR len(list_filter(w.subjects, lambda s: regexp_matches(s, getvariable('not_a_book_subjects'), 'i'))) > 0
        AS not_a_book,
    coalesce(r.ol_log, 0) AS ol_log,
    coalesce(r.ol_n, 0) AS ol_n,
    coalesce(r.ol_sum, 0) AS ol_sum,
    coalesce(g.gr_n, 0) AS gr_n,
    coalesce(g.gr_sum, 0) AS gr_sum
FROM ol_works w
JOIN work_authors wa ON wa.work = w.work
JOIN edition_facts e ON e.work = w.work AND e.has_isbn
LEFT JOIN edition_titles et ON et.work = w.work
LEFT JOIN reader_signs r ON r.work = w.work
LEFT JOIN gr_per_work g ON g.work = w.work
WHERE w.title IS NOT NULL
  AND len(wa.authors) > 0
  AND (coalesce(r.ol_log, 0) > 0 OR coalesce(r.ol_n, 0) > 0 OR coalesce(g.gr_n, 0) >= getvariable('min_goodreads_ratings'));
