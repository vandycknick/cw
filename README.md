# cw.rs

Your Swiss army knife for slicing and dicing AWS CloudWatch logs from the CLI.

`cw` is a fast, ergonomic CloudWatch Logs helper built in Rust.

## Features

- Fast, single Rust binary with no runtime dependencies
- Tail one or many log groups with optional stream prefixes
- CloudWatch filter patterns (`--filter`) and follow mode (`--follow`)
- Log Insights queries with a built-in editor flow
- Text or JSON output with optional timestamp, group, stream, and event id
- Local query history stored in SQLite for quick recall

## Installation

### From source (Cargo)

```bash
cargo install --path .
```

### From the Nix flake

```bash
nix build
./result/bin/cw --help
```

Or run directly:

```bash
nix run . -- --help
```

## Quick start

List groups, pick a stream, then tail it:

```bash
cw ls groups
cw ls streams /aws/lambda/my-function
cw tail /aws/lambda/my-function:prod --follow --timestamp
```

## Commands and CLI options

`cw tail --filter` accepts CloudWatch Logs filter patterns. See the AWS syntax reference:
https://docs.aws.amazon.com/AmazonCloudWatch/latest/logs/FilterAndPatternSyntax.html

The following is the exact output of `cw --help`:

```
Swiss army knife to query CloudWatch logs form the CLI.

Usage: cw [OPTIONS] <COMMAND>

Commands:
  ls
  tail
  query
  info

Options:
      --endpoint <ENDPOINT>
      --profile <PROFILE>    The AWS profile to use. By default it will try to get the profile from the AWS_PROFILE environment variable.
      --region <REGION>      The AWS region to use. By default it will read this value from AWS_REGION env var or from the region set in the provided profile.
  -h, --help                 Print help
  -v, --verbose...           Write verbose messages to stderr for debugging.
  -V, --version              Print version
```

## Authentication and configuration

`cw` relies on the AWS Rust SDK default credential and region providers. It will
use any of the standard sources supported by the SDK, including:

- `AWS_PROFILE` and shared config/credential files
- `AWS_REGION` or the region configured in your profile
- Environment variables such as `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY`
- AWS SSO and web identity flows supported by the SDK
- Role-based credentials when running on AWS (EC2, ECS, EKS, etc.)

If you need a custom CA bundle for TLS, set `AWS_CA_BUNDLE` to the path of the
certificate file.

If you run behind a proxy, the AWS SDK will honor `HTTP_PROXY`, `HTTPS_PROXY`,
and `NO_PROXY`.

## Data and logs

`cw` stores a small SQLite database for query history and a log file for runtime
output:

- Database: `${XDG_DATA_HOME:-~/.local/share}/cw/db.sqlite3`
- Logs: `${XDG_CACHE_HOME:-~/.local/cache}/cw/cw.log`

Run `cw info` to print the resolved paths for your machine.

## Usage examples

List log groups:

```bash
cw ls groups
```

List log streams for a group (optionally include expired streams):

```bash
cw ls streams /aws/lambda/my-function
cw ls streams /aws/lambda/my-function --show-expired
```

Tail logs from one or more groups (with optional stream prefix):

```bash
cw tail /aws/lambda/my-function
cw tail /aws/lambda/my-function:prod
cw tail /aws/lambda/my-function,/aws/lambda/other-service --follow
```

Tail with a filter pattern and extra metadata:

```bash
cw tail /aws/lambda/my-function --filter "ERROR" --timestamp --group-name
```

Filter pattern examples (standard, regex, JSON):

```bash
# Standard term match
cw tail /aws/lambda/my-function --filter "ERROR"

# Regex match (regex is wrapped in %...%)
cw tail /aws/lambda/my-function --filter "%Unauthorized%"

# JSON field match
cw tail /aws/lambda/my-function --filter "{ $.level = \"error\" }"
```

Query logs using a file and group selection:

```bash
cw query -g /aws/lambda/my-function -g /aws/lambda/other-service query.sql
```

Open an editor to write a query, then run it:

```bash
cw query -g /aws/lambda/my-function
```

Review query history:

```bash
cw query history
```

## Acknowledgements

- https://github.com/lucagrulla/cw
- https://github.com/runreveal/cwlogs
