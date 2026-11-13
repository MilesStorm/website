-- Functions every stage uses. Run before each stage (macros live in the database file too, but
-- a stage must not depend on an older build's definitions).

-- The number of an Open Library key: '/works/OL45804W' -> 45804. NULL when it isn't one.
CREATE OR REPLACE MACRO ol_num(k) AS
    TRY_CAST(regexp_extract(k, 'OL([0-9]+)[WMA]$', 1) AS BIGINT);

-- An ISBN with only its digits and X, upper-case.
CREATE OR REPLACE MACRO isbn_clean(s) AS upper(regexp_replace(s, '[^0-9Xx]', '', 'g'));

-- The ISBN-13 of a clean ISBN-10: prefix 978, recompute the check digit.
CREATE OR REPLACE MACRO isbn10_to_13(c) AS
    '978' || substr(c, 1, 9) || CAST(
        (10 - list_sum(list_transform(
            range(12),
            lambda isbn_pos_: CAST(substr('978' || substr(c, 1, 9), isbn_pos_ + 1, 1) AS INTEGER) * CASE WHEN isbn_pos_ % 2 = 0 THEN 1 ELSE 3 END
        )) % 10) % 10 AS VARCHAR);

-- Any written ISBN as an ISBN-13, NULL when it is neither a 10- nor a 13-digit ISBN.
CREATE OR REPLACE MACRO isbn13(s) AS
    CASE
        WHEN regexp_full_match(isbn_clean(s), '97[89][0-9]{10}') THEN isbn_clean(s)
        WHEN regexp_full_match(isbn_clean(s), '[0-9]{9}[0-9X]') THEN isbn10_to_13(isbn_clean(s))
    END;

-- A year (1000-2099) somewhere in a date string such as 'August 1965' or '1965-08-01'.
CREATE OR REPLACE MACRO year_of(s) AS
    TRY_CAST(nullif(regexp_extract(s, '(1[0-9]{3}|20[0-9]{2})', 1), '') AS SMALLINT);

-- A free-text series string from an Open Library edition, split into its name and optional
-- number: 'The Stormlight Archive #1', 'Harry Potter, 3', 'Discworld (5)',
-- 'Mistborn, book 2', 'The Expanse ; bk. 1', 'Dune Chronicles, Vol. 3'.
CREATE OR REPLACE MACRO series_re_number() AS
    '^(.*?)[\s,;:.(\[-]*(?:#|no\.?|nr\.?|n°|vol\.?|volume|book|bk\.?|part|tome|band|libro|livre)\s*([0-9]+(?:\.[0-9]+)?)\s*[)\]]?\s*$';
CREATE OR REPLACE MACRO series_re_comma() AS '^(.*?)\s*[,;]\s*([0-9]+(?:\.[0-9]+)?)\s*$';
CREATE OR REPLACE MACRO series_re_paren() AS '^(.*?)\s*\(([0-9]+(?:\.[0-9]+)?)\)\s*$';
CREATE OR REPLACE MACRO series_parts(v, re) AS
    {'name': trim(regexp_extract(v, re, 1, 'i'), ' ,;:.-'), 'number': TRY_CAST(regexp_extract(v, re, 2, 'i') AS DOUBLE)};
CREATE OR REPLACE MACRO series_parse(s) AS
    CASE
        WHEN regexp_matches(trim(s), series_re_number(), 'i') THEN series_parts(trim(s), series_re_number())
        WHEN regexp_matches(trim(s), series_re_comma()) THEN series_parts(trim(s), series_re_comma())
        WHEN regexp_matches(trim(s), series_re_paren()) THEN series_parts(trim(s), series_re_paren())
        ELSE {'name': trim(trim(s), ' ,;:.-'), 'number': NULL::DOUBLE}
    END;

-- A series name made comparable: lower case without accents or punctuation, no leading 'the',
-- without the words series, trilogy, saga, chronicles.
CREATE OR REPLACE MACRO series_norm(n) AS
    trim(regexp_replace(
        regexp_replace(
            regexp_replace(
                trim(regexp_replace(regexp_replace(regexp_replace(lower(strip_accents(n)), '[''’]', '', 'g'), '[^a-z0-9]', ' ', 'g'), '\s+', ' ', 'g')),
                '^the ', ''),
            '\b(series|trilogy|saga|chronicles)\b', ' ', 'g'),
        '\s+', ' ', 'g'));
