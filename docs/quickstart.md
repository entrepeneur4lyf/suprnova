---
title: "Quickstart"
description: "Start building web apps in less than 5 mins"
---

## Get started in three steps

This quickstart guide assumes that you already have Rust and Cargo up and running on your machine.

> **Note:**
>
> [Look at this guide ](https://rust-lang.org/learn/get-started/) if you want to know how to setup Rust and Cargo on your machine.


### Step 1: Install the suprnova-cli

Install the suprnova-cli on your machine via Cargo using the following command:

```bash Terminal
cargo install suprnova-cli
```

### Step 2:  Create your project using suprnova-cli

Run the following command to create your project. Replace the `todo-app` with your project name

```bash Terminal
suprnova new todo-app
```

The ineractive shell will ask some project specific questions like Project Name, Author etc. Answer all the questions and then the cli will generate the project files for you

### Step 3: Run migrations

Before starting the server, run the database migrations to set up the users and sessions tables:

```bash Terminal
cd todo-app
suprnova migrate
```

### Step 4: Start the web server

Now start the web server using the suprnova serve command:

```bash Terminal
suprnova serve
```

This will start the web server and in the terminal you will see the URL on which your web server is running. Go to the url and you will find your app up and running!

## What's Included

Your new suprnova project comes with batteries included:

- **Authentication** - Login, registration, and logout endpoints with session-based auth
- **Protected routes** - Dashboard page that requires authentication
- **CSRF protection** - Automatic protection against cross-site request forgery
- **Database migrations** - Users and sessions tables ready to go
- **React/Inertia frontend** - Pre-configured with TypeScript and Tailwind CSS

Visit `/login` to see the login page, or `/register` to create a new account.