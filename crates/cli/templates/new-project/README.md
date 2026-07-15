# {{ project_name }}

Start the project with:

```bash
ryvus start
```

Ryvus loads configuration from `.env`. Process environment variables override values in that file.

The generated project uses in-memory execution state by default:

```dotenv
RYVUS_EXECUTION_STORE=memory
```

To persist execution state in an already-created and migrated PostgreSQL database, use:

```dotenv
RYVUS_EXECUTION_STORE=postgres
DATABASE_URL=postgres://user:password@localhost:5432/ryvus
```

Run migrations explicitly before starting with PostgreSQL:

```bash
ryvus database migrate
```

Ryvus does not create, start, or migrate PostgreSQL automatically.
