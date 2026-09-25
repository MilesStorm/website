-- The camera frame of a roll, kept apart so listing samples never loads pictures.
-- Same key as its sample: roll_image:[username, roll_id].
DEFINE TABLE roll_image SCHEMAFULL;
DEFINE FIELD username ON roll_image TYPE string;
DEFINE FIELD jpeg ON roll_image TYPE bytes;
DEFINE FIELD created_at ON roll_image TYPE datetime DEFAULT time::now() READONLY;
