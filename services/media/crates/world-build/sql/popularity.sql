-- Stage popularity (02-world-data.md §3). Variable: prior_weight (m).

-- Open Library reading-log counts on the Goodreads scale: least squares
-- ln(1 + gr_n) = a + b ln(1 + ol_log) over works that have both.
CREATE OR REPLACE TABLE popularity_fit AS
SELECT coalesce(regr_intercept(ln(1 + gr_n), ln(1 + ol_log)), 0) AS a,
       coalesce(regr_slope(ln(1 + gr_n), ln(1 + ol_log)), 1) AS b,
       count(*) FILTER (WHERE gr_n > 0 AND ol_log > 0) AS fitted_on
FROM catalogue
WHERE gr_n > 0 AND ol_log > 0;

CREATE OR REPLACE TABLE popularity AS
WITH mu AS (
    SELECT (sum(gr_sum) + sum(ol_sum)) / nullif(sum(gr_n) + sum(ol_n), 0) AS mu FROM catalogue
)
SELECT c.work,
       -- est_n = expm1(a + b ln(1 + ol_log)); popularity = ln(1 + max(gr_n, est_n)).
       ln(1 + greatest(c.gr_n, CASE WHEN c.ol_log > 0 THEN exp(f.a + f.b * ln(1 + c.ol_log)) - 1 ELSE 0 END)) AS popularity,
       (c.gr_sum + c.ol_sum + getvariable('prior_weight') * mu.mu) / (c.gr_n + c.ol_n + getvariable('prior_weight')) AS quality
FROM catalogue c, popularity_fit f, mu;
