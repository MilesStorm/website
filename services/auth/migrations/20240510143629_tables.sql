-- Add migration script here
create table if not exists users (
  id bigserial primary key,
  username varchar(255) not null unique,
  password varchar(255),
  email varchar(255) unique,
  access_token varchar(255),
  created_at timestamp default current_timestamp
);
