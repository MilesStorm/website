-- "Delete all pictures I've shared": who and when (nothing else), so the training
-- pull tool also removes copies it already downloaded.
DEFINE TABLE dataset_deletion SCHEMAFULL;
DEFINE FIELD username ON dataset_deletion TYPE string;
DEFINE FIELD at ON dataset_deletion TYPE datetime DEFAULT time::now() READONLY;
