-- One settled roll kept for training: roll_sample:[username, roll_id].
-- Whether a user shares roll pictures is stored with their account by the auth
-- service (PostgreSQL table dataset_consent), not here.
DEFINE TABLE roll_sample SCHEMAFULL;
DEFINE FIELD username ON roll_sample TYPE string;
DEFINE FIELD roll_id ON roll_sample TYPE string;
-- Why it was kept automatically (opted-in users only): a random sample, or the
-- model was unsure. NONE when it was only kept because the user flagged it.
DEFINE FIELD auto_reason ON roll_sample TYPE option<string>
    ASSERT $value = NONE OR $value IN ['sampled', 'unsure'];
-- The user pressed "flag as wrong roll"; user_values are their optional corrections
-- per die, left to right (null = left empty). Hints only: labels are set in review.
DEFINE FIELD flagged ON roll_sample TYPE bool DEFAULT false;
DEFINE FIELD flagged_at ON roll_sample TYPE option<datetime>;
DEFINE FIELD user_values ON roll_sample TYPE option<array<string | null>>;
-- The roll event as the model reported it, and the last frame's raw detections.
DEFINE FIELD roll ON roll_sample TYPE object FLEXIBLE;
DEFINE FIELD frame ON roll_sample TYPE option<object> FLEXIBLE;
DEFINE FIELD image ON roll_sample TYPE record<roll_image>;
-- Which dice head produced the prediction: its release tag (DICE_MODEL_ID, e.g.
-- dice-head-v1; the weights' SHA-256 is pinned next to it in the ai_pipeline Dockerfile).
DEFINE FIELD model ON roll_sample TYPE string;
-- new → reviewed (labels decided by the owner) or rejected (unusable).
DEFINE FIELD review_status ON roll_sample TYPE string DEFAULT 'new'
    ASSERT $value IN ['new', 'reviewed', 'rejected'];
DEFINE FIELD created_at ON roll_sample TYPE datetime DEFAULT time::now() READONLY;
DEFINE FIELD updated_at ON roll_sample TYPE datetime VALUE time::now();
DEFINE INDEX roll_sample_user ON roll_sample FIELDS username;
DEFINE INDEX roll_sample_review ON roll_sample FIELDS review_status, created_at;
