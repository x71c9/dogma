# dogma

Opinionated CLI that bridges secrets from any vault backend and infrastructure outputs into sops-encrypted files deployed to remote machines, driven by a central `dogma.yml` config.

## What it does

dogma solves the problem of getting secrets onto machines without ever storing them in plaintext. It pulls secret values from two sources:

- **Vault backends** — environment variables, `pass`, or any custom plugin
- **Infrastructure outputs** — OpenTofu / Terraform output values (e.g. a freshly provisioned server's IP or domain name)

It then encrypts them per-machine with [sops](https://github.com/getsops/sops) and deploys them. On NixOS, a ready-made module mounts each secret at `/run/secrets/<group>/<field>`. On any other OS, the encrypted sops files are yours to use however you need.

```
dogma.yml
    │
    ├── vault refs  ──────────────────────────────────┐
    │   (envvar / pass / custom)                      │
    │                                                 ▼
    └── infra outputs  ──────────────────────────► encrypt with sops
        (OpenTofu / Terraform)                        │
                                                      ▼
                                              deploy to machines
                                              /run/secrets/<group>/<field>
```

## Minimal example

A single web server that needs a database password and a Stripe key:

```yaml
# dogma.yml

admin:
  - age: age1xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
  - gpg: 3246EC6F403D8F3403E34A90BCDABC277B1ED8AB

env:
  - dev
  - staging
  - prod

machines:
  web:
    hostname: web-{env}
    user: deployer
    ip:
      prod: 1.2.3.4
    secrets:
      - database
      - stripe

secrets:
  database:
    password:
      from: vault
      ref: db-password

  stripe:
    webhook_secret:
      from: infra
      output: stripe_webhook_secret
      unit: payments

vault:
  db-password:
    envvar: DB_PASSWORD
```

Run:

```sh
dogma deploy prod
```

dogma resolves `DB_PASSWORD` from your environment and `stripe_webhook_secret` from the OpenTofu `payments` unit output, encrypts them with sops for the `web` machine, and deploys. On the server, the secrets are available at:

```
/run/secrets/database/password
/run/secrets/stripe/secret_key
```

## Real-world example

Two servers across two environments. The database URL comes from an infrastructure output (the DB was just provisioned by OpenTofu), the API keys come from `pass`:

```yaml
admin:
  - age: age1xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
  - gpg: 3246EC6F403D8F3403E34A90BCDABC277B1ED8AB

env:
  - staging
  - prod

machines:
  api:
    hostname: myapp-{env}-api
    user: deployer
    ip:
      from: infra
      output: api_server_ip
      unit: servers
    secrets:
      - database
      - payments
      - auth

  worker:
    hostname: myapp-{env}-worker
    user: deployer
    ip:
      from: infra
      output: worker_server_ip
      unit: servers
    secrets:
      - database

secrets:
  database:
    url:
      from: infra
      output: database_url
      unit: servers

  payments:
    stripe_secret_key:
      from: vault
      ref: stripe-secret-key
    stripe_webhook_secret:
      from: vault
      ref: stripe-webhook-secret

  auth:
    jwt_secret:
      from: vault
      ref: jwt-secret
    github_oauth_secret:
      from: vault
      ref: github-oauth-secret

vault:
  stripe-secret-key:
    envvar: STRIPE_SECRET_KEY
    pass: stripe.com/myapp/{env}/secret-key

  stripe-webhook-secret:
    envvar: STRIPE_WEBHOOK_SECRET
    pass: stripe.com/myapp/{env}/webhook-secret

  jwt-secret:
    envvar: JWT_SECRET
    pass: myapp/{env}/jwt-secret

  github-oauth-secret:
    envvar: GITHUB_OAUTH_SECRET
    pass: github.com/myapp/{env}/oauth-secret
```

Deploy to staging:

```sh
dogma deploy staging
```

## Core concepts

### dogma.yml

A single file describes your entire deployment:

| Section    | Purpose |
|------------|---------|
| `admin`    | GPG/age/SSH keys that can decrypt all secrets |
| `env`      | List of environments (`dev`, `staging`, `prod`, …) |
| `machines` | Machines with hostname, user, IP source, and secret groups |
| `secrets`  | Secret groups — each leaf is `from: vault` or `from: infra` |
| `vault`    | Named vault entries with one or more backends |
| `infra`    | Infrastructure CLI (tofu/terraform), path, and credentials |
| `nix`      | Nix flake root path (NixOS only) |

### Secret groups

Secrets are organized into named groups. Each leaf is resolved at deploy time from a vault backend or an infrastructure output:

```yaml
secrets:
  database:
    password:
      from: vault
      ref: db-password     # looks up 'db-password' in the vault section
    host:
      from: infra
      output: db_host      # reads the 'db_host' output from OpenTofu
      unit: database
```

### Vault backends

Vault entries can have multiple backends. dogma picks the one active for the current run (`DOGMA_VAULT`, default: `envvar`):

```yaml
vault:
  db-password:
    envvar: DB_PASSWORD                        # reads $DB_PASSWORD
    pass: myapp/{env}/database/password        # reads from pass store
```

Custom backends are supported: drop a `vault-<name>.sh` on your `PATH` and set `DOGMA_VAULT=<name>`.

### Environments

Use `{env}` as a placeholder anywhere in `dogma.yml` — hostnames, vault paths, infra outputs. dogma expands it to the target environment at deploy time:

```yaml
machines:
  api:
    hostname: myapp-{env}-api

vault:
  db-password:
    pass: myapp/{env}/database/password
```

## Commands

```sh
dogma deploy [--skip-infra] [--skip-sops] [--refetch] <env> [<host>]
dogma apply  <env> <unit>    # tofu init + apply
dogma destroy <env> <unit>   # tofu init + destroy
```

### deploy pipeline

1. **Normalize + validate** `dogma.yml` — expand `{env}` placeholders, check all refs exist
2. **Refresh infra cache** — fetch OpenTofu outputs into `.dogma/cache/<env>.json`
3. **Generate `.sops.yaml`** — fetch SSH host ed25519 keys, convert to age, write creation rules
4. **Generate `secrets.nix`** — write the flat secret list consumed by the NixOS module
5. **Encrypt secrets** — resolve each leaf, write YAML, encrypt with sops per machine
6. **Deploy** — run `nixos-rebuild switch` (or your own deploy step) targeting each host

## NixOS integration

Set `dogma.machine` once per host. The module auto-mounts every secret at `/run/secrets/<group>/<field>`:

```nix
# hosts/api/default.nix
{ config, ... }: {
  dogma.machine = "api";
}
```

Override owner and mode per secret:

```nix
dogma.secrets."database/password".owner = "myapp";
dogma.secrets."database/password".mode  = "0440";
```

### Containers

Secrets are bind-mounted into declarative NixOS containers automatically:

```nix
hnContainers.my-service = {
  allSecrets = true;   # or: secrets = [ "database/password" ];
  config = { ... };
};
```

The `secrets-env` wrapper reads `/run/secrets/**` and exports each file as an uppercase environment variable — useful as an `ExecStart` prefix.

## Non-NixOS use

dogma does not require NixOS. The encrypted sops files written per machine are standard sops YAML files and can be decrypted and consumed by any tooling that speaks sops.

