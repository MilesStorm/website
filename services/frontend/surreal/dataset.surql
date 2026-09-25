-- Dice training-data store (SurrealDB 3.x). Run as root; safe to re-run.
-- The website's own login (`dice`, limited to this database) is created separately
-- so its password never lands in git; see README.md in this folder.

DEFINE NAMESPACE IF NOT EXISTS milesstorm;
USE NS milesstorm;
DEFINE DATABASE IF NOT EXISTS arcane;
USE NS milesstorm DB arcane;

-- Whether a user shares roll pictures is stored with their account by the auth
-- service (PostgreSQL table dataset_consent), not here.

-- The camera frame of a roll, kept apart so listing samples never loads pictures.
-- Same key as its sample: roll_image:[username, roll_id].
DEFINE TABLE IF NOT EXISTS roll_image SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS username ON roll_image TYPE string;
DEFINE FIELD IF NOT EXISTS jpeg ON roll_image TYPE bytes;
DEFINE FIELD IF NOT EXISTS created_at ON roll_image TYPE datetime DEFAULT time::now() READONLY;

-- One settled roll kept for training: roll_sample:[username, roll_id].
DEFINE TABLE IF NOT EXISTS roll_sample SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS username ON roll_sample TYPE string;
DEFINE FIELD IF NOT EXISTS roll_id ON roll_sample TYPE string;
-- Why it was kept automatically (opted-in users only): a random sample, or the
-- model was unsure. NONE when it was only kept because the user flagged it.
DEFINE FIELD IF NOT EXISTS auto_reason ON roll_sample TYPE option<string>
    ASSERT $value = NONE OR $value IN ['sampled', 'unsure'];
-- The user pressed "flag as wrong roll"; user_values are their optional corrections
-- per die, left to right (null = left empty). Hints only: labels are set in review.
DEFINE FIELD IF NOT EXISTS flagged ON roll_sample TYPE bool DEFAULT false;
DEFINE FIELD IF NOT EXISTS flagged_at ON roll_sample TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS user_values ON roll_sample TYPE option<array<string | null>>;
-- The roll event as the model reported it, and the last frame's raw detections.
DEFINE FIELD IF NOT EXISTS roll ON roll_sample TYPE object FLEXIBLE;
DEFINE FIELD IF NOT EXISTS frame ON roll_sample TYPE option<object> FLEXIBLE;
DEFINE FIELD IF NOT EXISTS image ON roll_sample TYPE record<roll_image>;
-- Which dice head produced the prediction: its release tag (DICE_MODEL_ID, e.g.
-- dice-head-v1; the weights' SHA-256 is pinned next to it in the ai_pipeline Dockerfile).
DEFINE FIELD IF NOT EXISTS model ON roll_sample TYPE string;
-- new → reviewed (labels decided by the owner) or rejected (unusable).
DEFINE FIELD IF NOT EXISTS review_status ON roll_sample TYPE string DEFAULT 'new'
    ASSERT $value IN ['new', 'reviewed', 'rejected'];
DEFINE FIELD IF NOT EXISTS created_at ON roll_sample TYPE datetime DEFAULT time::now() READONLY;
DEFINE FIELD IF NOT EXISTS updated_at ON roll_sample TYPE datetime VALUE time::now();
DEFINE INDEX IF NOT EXISTS roll_sample_user ON roll_sample FIELDS username;
DEFINE INDEX IF NOT EXISTS roll_sample_review ON roll_sample FIELDS review_status, created_at;

-- "Delete all pictures I've shared": who and when (nothing else), so the training
-- pull tool also removes copies it already downloaded.
DEFINE TABLE IF NOT EXISTS dataset_deletion SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS username ON dataset_deletion TYPE string;
DEFINE FIELD IF NOT EXISTS at ON dataset_deletion TYPE datetime DEFAULT time::now() READONLY;
