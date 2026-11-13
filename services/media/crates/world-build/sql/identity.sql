-- Stage identity: each Goodreads work -> exactly one Open Library work (02-world-data.md §3).

-- A Goodreads book links to an Open Library work through an edition with its ISBN-13, or with
-- its Goodreads book id in identifiers.goodreads. One link per (book, work).
CREATE OR REPLACE TABLE gr_links AS
SELECT DISTINCT b.book_id, b.work_id AS gr_work, l.work AS ol_work, b.ratings_count
FROM gr_books b
JOIN (
    SELECT DISTINCT unnest(isbns) AS isbn, work FROM ol_editions WHERE work IS NOT NULL
) l ON l.isbn = b.isbn13
UNION
SELECT DISTINCT b.book_id, b.work_id, g.work, b.ratings_count
FROM gr_books b
JOIN (
    SELECT DISTINCT unnest(goodreads) AS book_id, work FROM ol_editions WHERE work IS NOT NULL
) g ON g.book_id = b.book_id;

-- The Open Library work that gets the most links, each weighted by its book's ratings + 1.
-- Ties: the smaller key. Several Goodreads works may land on one Open Library work.
CREATE OR REPLACE TABLE gr_work_map AS
SELECT gr_work, ol_work
FROM (
    SELECT gr_work, ol_work,
           row_number() OVER (PARTITION BY gr_work ORDER BY sum(ratings_count + 1) DESC, ol_work ASC) AS rank
    FROM gr_links
    WHERE gr_work IS NOT NULL
    GROUP BY gr_work, ol_work
)
WHERE rank = 1;

-- Goodreads numbers per Open Library work (sums over its Goodreads works).
CREATE OR REPLACE TABLE gr_per_work AS
SELECT m.ol_work AS work,
       sum(w.ratings_count) AS gr_n,
       sum(w.ratings_sum) AS gr_sum,
       min(w.year) AS gr_year
FROM gr_work_map m JOIN gr_works w ON w.work_id = m.gr_work
GROUP BY m.ol_work;
