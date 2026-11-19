-- Stage interactions (02-world-data.md §3 "Stages added in M2", 04-benchmark.md §2): Goodreads
-- ratings on Open Library works, the benchmark's users, and the factorization's training pairs.
-- Variables: gr_interactions (the downloaded file), min_eligible_ratings, test_users, gate_users,
-- tuning_users, split_seed, min_user_likes, min_work_likes, cold_from_year.

-- Goodreads user ids are 32 hex characters; a dense number each, in id order.
-- Dates look like "Fri Sep 08 10:44:24 -0700 2017"; date_updated when date_added is unreadable.
-- The offset is dropped (a TIMESTAMP, local time): hours do not matter for ordering a reader's
-- books, and time zones need DuckDB's ICU extension, which the build does not load.
CREATE OR REPLACE MACRO gr_date(d) AS
    try_strptime(regexp_replace(d, ' [+-][0-9]{4} ', ' '), '%a %b %d %H:%M:%S %Y');

CREATE OR REPLACE TABLE gr_rated AS
SELECT user_id, TRY_CAST(book_id AS BIGINT) AS book_id, rating,
       coalesce(gr_date(date_added), gr_date(date_updated)) AS added
FROM read_json(getvariable('gr_interactions'), format = 'newline_delimited', compression = 'gzip',
               columns = {'user_id': 'VARCHAR', 'book_id': 'VARCHAR', 'rating': 'INTEGER',
                          'date_added': 'VARCHAR', 'date_updated': 'VARCHAR'})
WHERE rating BETWEEN 1 AND 5;

CREATE OR REPLACE TABLE gr_users AS
SELECT user_id, (row_number() OVER (ORDER BY user_id))::INTEGER AS user
FROM (SELECT DISTINCT user_id FROM gr_rated);

-- One row per (user, Open Library work): the highest rating and the earliest date. Every row
-- names its source, so a second reading-data source adds rows here (02-world-data.md §8).
CREATE OR REPLACE TABLE ratings AS
SELECT 'goodreads' AS source, u.user, m.ol_work AS work, max(r.rating)::TINYINT AS rating, min(r.added) AS added
FROM gr_rated r
JOIN gr_users u ON u.user_id = r.user_id
JOIN gr_books b ON b.book_id = r.book_id
JOIN gr_work_map m ON m.gr_work = b.work_id
GROUP BY u.user, m.ol_work;

DROP TABLE gr_rated;

-- The benchmark's users: eligible users in a seeded order. The first test_users are test users
-- (the first gate_users of them the monthly gate subset), the next tuning_users tuning users.
CREATE OR REPLACE TABLE bench_users AS
WITH eligible AS (
    SELECT user FROM ratings GROUP BY user HAVING count(*) >= getvariable('min_eligible_ratings')
),
ordered AS (
    SELECT e.user, row_number() OVER (ORDER BY md5(u.user_id || ':' || getvariable('split_seed')), e.user) AS k
    FROM eligible e JOIN gr_users u ON u.user = e.user
)
SELECT user,
       CASE WHEN k <= getvariable('test_users') THEN 'test' ELSE 'tuning' END AS role,
       k <= getvariable('gate_users') AS gate
FROM ordered
WHERE k <= getvariable('test_users') + getvariable('tuning_users');

-- Liked pairs (4 or 5 stars) without the test users. Users with too few likes and works liked
-- too rarely are dropped, and the filter is applied twice (03-engine.md §2).
CREATE OR REPLACE MACRO filtered_likes(excluded_from_year) AS TABLE
WITH likes AS (
    SELECT r.user, r.work
    FROM ratings r
    LEFT JOIN catalogue c ON c.work = r.work
    WHERE r.rating >= 4
      AND r.user NOT IN (SELECT user FROM bench_users WHERE role = 'test')
      AND coalesce(c.year, 0) < excluded_from_year
),
once AS (
    SELECT * FROM likes
    WHERE user IN (SELECT user FROM likes GROUP BY user HAVING count(*) >= getvariable('min_user_likes'))
      AND work IN (SELECT work FROM likes GROUP BY work HAVING count(*) >= getvariable('min_work_likes'))
)
SELECT * FROM once
WHERE user IN (SELECT user FROM once GROUP BY user HAVING count(*) >= getvariable('min_user_likes'))
  AND work IN (SELECT work FROM once GROUP BY work HAVING count(*) >= getvariable('min_work_likes'));

CREATE OR REPLACE TABLE train_likes AS SELECT * FROM filtered_likes(32767);

-- The cold-book test (04-benchmark.md §4): the same without works first published from
-- cold_from_year on.
CREATE OR REPLACE TABLE train_likes_cold AS SELECT * FROM filtered_likes(getvariable('cold_from_year'));
