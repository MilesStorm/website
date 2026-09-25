-- Each account's profile picture, as the website re-encoded it (256x256 JPEG, no
-- hidden data such as location). One row per account: profile_picture:<user id>,
-- the permanent id from auth (usernames can't be relied on to stay unique over time).
DEFINE TABLE profile_picture SCHEMAFULL;
DEFINE FIELD jpeg ON profile_picture TYPE bytes;
DEFINE FIELD updated_at ON profile_picture TYPE datetime DEFAULT ALWAYS time::now();
