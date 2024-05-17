CREATE OR REPLACE FUNCTION insert_user(v_username VARCHAR, v_email VARCHAR, v_password VARCHAR, v_access_token VARCHAR DEFAULT NULL)
RETURNS TABLE(id BIGINT, username VARCHAR, email VARCHAR, password VARCHAR, access_token VARCHAR) AS $$
BEGIN
    -- Check if the username already exists
    IF EXISTS (SELECT 1 FROM users AS u WHERE u.username = v_username) THEN
        RAISE EXCEPTION 'UserAlreadyExists';
    END IF;

    -- Check if the email already exists
    IF EXISTS (SELECT 1 FROM users AS u WHERE u.email = v_email) THEN
        RAISE EXCEPTION 'EmailAlreadyInUse';
    END IF;

    -- If no conflicts, insert the new user
    RETURN QUERY
    INSERT INTO users (username, email, password, access_token)
    VALUES (v_username, v_email, 
            CASE WHEN v_access_token IS NOT NULL THEN NULL ELSE v_password END, 
            v_access_token)
    RETURNING users.id AS id, users.username, users.email, users.password AS password, users.access_token AS access_token;
END;
$$ LANGUAGE plpgsql;
