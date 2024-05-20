-- Add migration script here
create table if not exists users (
  id bigserial primary key,
  username varchar(255) not null unique,
  password varchar(255),
  email varchar(255) unique,
  access_token varchar(255),
  created_at timestamp default current_timestamp
);

-- insert into users (id, username, email, password) values (1, 'yousof', 'yousof777@gmail.com', '$argon2i$v=19$m=16,t=2,p=1$WXB3ZVNTM2dOSmJIYXNlMw$tDUgH5HWPTWTn+U4qMeNEQ');
