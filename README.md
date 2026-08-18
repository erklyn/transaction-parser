# Payment Engine

Reads a CSV of transactions, applies them to per-client accounts, and writes the
resulting balances to stdout.

```console
$ cargo build
$ cargo run -- transactions.csv > accounts.csv
```

- There's a sample `transactions.csv` in the repository root, so the command
  above works as written. It's the example from the brief plus a few rows that
  exercise a held dispute and a chargeback.
- Warnings go to stderr, so stdout is always clean CSV even when rows are
  skipped.

## Layout

| File | What's in it |
| --- | --- |
| `src/money.rs` | The amount type, its precision, and exact arithmetic |
| `src/record.rs` | The CSV row, and the `Transaction` it parses into |
| `src/account.rs` | One client's balances and the six ways they move |
| `src/engine.rs` | The ledger — accounts, recorded transactions, the rules |
| `src/csv_io.rs` | CSV in, CSV out |
| `src/error.rs` | Every reason a row is skipped, and the two that stop a run |
| `src/main.rs` | Arguments and stdout |

Two boundaries I cared about:

- **`engine` never mentions CSV, and `csv_io` never mentions the rules.** The
  engine's entry point is `Engine::apply(Transaction)`, so the rules are
  testable without a file anywhere in sight.
- **A row becomes a `Transaction` before the engine sees it.** Everything that
  can be wrong with the *row* is settled by then, which is why the ledger does
  no input validation of its own.

## Where I leaned on the type system

A few things I'd rather have the compiler enforce than remember to test.

- **`Transaction` is an enum whose variants carry only what they need.** A
  `Deposit` has an `Amount`, not an `Option<Amount>`; a `Dispute` has no amount
  field at all. "A deposit with no amount" and "a dispute with one" aren't
  representable at the engine boundary, so there's nothing to check for and
  nothing to unwrap.
- **`Account` doesn't store a total.** `total()` is `available + held`, computed
  on demand — no third field to forget to update, and no way for the figures to
  contradict each other.
- **Balances are private, and only move through six `pub(crate)` methods** —
  `credit`, `debit`, `move_available_to_held`, `move_held_to_available`,
  `add_held`, `remove_held` — named so the two inverse pairs are obvious at a
  call site.
- **Money is `rust_decimal`, not `f64`.** In floating point `0.1 + 0.2` is
  `0.30000000000000004`, and over a long run a balance drifts away from the sum
  of its inputs. Bad property for a ledger.
- **Balance arithmetic goes through `money::exact_add` / `exact_sub`.** They
  check for overflow, and also that the result kept full precision — `Decimal`
  has a floating scale, so at extreme magnitudes it will trade decimal places
  for range rather than fail. A row that would land there is skipped like any
  other bad row, so a balance is never quietly rounded.
- **Dispute state is a three-state enum, not a bool**, so "charged back" is
  distinguishable from "not currently disputed".
- **Accounts live in a `BTreeMap`**, which makes reporting them in client order
  a property of the structure rather than a sort someone could delete.

## Reading the spec

The spec explains disputes in terms of a deposit and doesn't say what disputing
a **withdrawal** should do. I treated it as a claim for a refund of money that
has already left the account.

| | Dispute | Resolve | Chargeback |
| --- | --- | --- | --- |
| **Deposit** | available −amt, held +amt | held −amt, available +amt | held −amt (total falls) |
| **Withdrawal** | held +amt (total rises) | held −amt (total falls) | held −amt, available +amt |

- Read down either column: a resolve exactly undoes its dispute, and a
  chargeback reverses the original movement.
- The deposit column is the spec's literal wording.
- The withdrawal column departs from a literal reading in one way — `total`
  changes when the dispute opens. That's forced: the disputed funds aren't in the
  account to be shuffled from `available` into `held`, so holding a pending
  refund has to come from somewhere.
- The alternative is applying the deposit rule anyway, which drives `available`
  negative on the dispute and then leaves the chargeback with nothing to do.
  That doesn't describe anything a bank does, so I went the other way.

## Assumptions

Roughly in order of how much they'd surprise someone.

- **A rejected row leaves no trace.** `Engine::apply` returns `Err` without
  moving a balance, recording a transaction, or creating an account for a client
  it hadn't seen. A client who never appears in an accepted row doesn't appear in
  the output at all.
- **A locked account accepts nothing else, ever.** After a chargeback every later
  row for that client is skipped — deposits, withdrawals, and the whole dispute
  lifecycle, including resolving a dispute that was already open when the freeze
  landed. I read "immediately frozen" strictly and treated unfreezing as
  administrative, outside this program.
- **Both deposits and withdrawals can be disputed.** Both are recorded, and a
  dispute can reference either.
- **Duplicate transaction IDs are replays, so the first one wins.** IDs are
  globally unique per the spec, so a repeat means something went wrong upstream.
  Overwriting would let a later row repoint an open dispute at a different
  amount, which is close to the fraud the brief opens with.
- **A dispute has to come from the client who owns the transaction.** A
  mismatched client ID is skipped rather than applied to either account.
- **Deposits and withdrawals need an amount greater than zero.** A negative
  deposit is just a withdrawal that skips the sufficient-funds check.
- **A resolved dispute can be raised again; a chargeback is final.**
- **More than four decimal places gets rounded, not rejected** (half to even).
  The spec says four places can be assumed, so anything longer is out of contract
  and being forgiving seemed better than refusing. Consequence worth knowing:
  anything under `0.00005` rounds to zero and is then skipped as non-positive.
- **`available` can go negative.** Deposit, withdraw the proceeds, then have the
  deposit charged back — the client genuinely owes that money, and a negative
  balance is the honest record of it. Refusing the chargeback would be worse.
- **The header must name `type`, `client`, `tx` and `amount`**, in any order and
  any case. A header that doesn't is fatal rather than per-row, because otherwise
  every row fails identically and the program still exits 0 with an empty table.
  Type names are matched case-insensitively too.
- **A row with more fields than the header is skipped, not truncated.** Short
  rows are fine — a dispute can stop after `tx` — but taking the first four
  fields of a long row would post `1,000` as `1`.
- **Rows are applied in file order**, which the spec allows.
- **Output is sorted by client**, which the spec doesn't require. It costs
  nothing here and makes the output diffable.
- **Trailing zeros are trimmed**, so `2` rather than `2.0000`, matching the
  second example in the brief.

## Tests

```console
$ cargo test
```

74 of them, in four layers.

- **Unit tests next to what they cover.** `account.rs` for the balance
  primitives — overflow, precision loss, and that each pair of movements really
  is an inverse. `money.rs` for precision and rendering. `record.rs` for parsing:
  the five type names, case and whitespace handling, missing and non-positive
  amounts, rounding, and that an amount on a dispute row is ignored.
- **Engine tests**, the bulk of it:
  - every combination of {deposit, withdrawal} × {dispute, resolve, chargeback};
  - every way a transaction can be refused — unknown transaction, wrong client,
    double dispute, resolving something undisputed, charging back after a
    resolve, duplicate IDs, insufficient funds, a balance that can't be tracked
    exactly, and a locked account turning away each of the five kinds in turn;
  - the fraud story from the brief, several clients' disputes open at once, and a
    check that no rejected transaction leaves an account behind.
- **End-to-end tests** over sample data in `tests/data/`. Each case is two files,
  `<name>.csv` and `<name>.expected.csv`, compared byte for byte.
  - `spec_example.csv` is the brief's own example and reproduces its output
    exactly.
  - The others cover the dispute lifecycle, the fraud reversal, precision and
    spacing, and interleaved clients.
  - One file has thirteen rejected rows spanning ten distinct reasons, and the
    test asserts *which* rule caught each row, not just the final balances.
  - A separate test checks the directory and the case list agree, so a fixture
    can't quietly go missing.
- **CLI tests** (`tests/cli.rs`) run the actual binary: the documented output for
  the sample input, warnings on stderr and never stdout, and a non-zero exit with
  empty stdout for no argument, two arguments, a missing file and a directory.

One detail worth flagging: balances are asserted as four independent numbers,
with `total` written out rather than computed from the other two. An earlier
version asserted `total == available + held`, which is exactly what
`Account::total()` does — so it could never fail. Easy trap.

Also covered along the way: empty files, files where every row is rejected,
dispute rows written with three fields instead of four, columns in a different
order and case, a header missing required columns, a row longer than the header,
and row numbers staying correct across CRLF endings and blank lines.

## Errors

Two kinds, handled differently.

**Anything wrong with a single row is reported and skipped, and the run carries
on:**

```
warning: row 5: client 2 has 2.0 available but tried to withdraw 3.0
```

- That number counts **data rows, not physical lines**. I started with line
  numbers and found they went wrong on CRLF files and files with blank lines,
  because the underlying counter tracks `\n` bytes and snapshots position before
  consuming the terminator. Row numbers are always right, and they're closer to
  what someone means by "the third transaction".
- Row-level failures split in two: `ParseError` for a row that doesn't describe
  a transaction, `Reject` for one the ledger won't accept. Each set is
  enumerated in one place, and each variant carries the values needed to explain
  itself.
- `Engine::apply` leaves the ledger untouched on `Err`, so a skipped row can't
  half-apply.
- A feed that stops at the first bad line is worse than one that processes the
  other 99,999 and tells you what it dropped.

**Everything else is fatal and loud** — non-zero exit, message on stderr,
nothing on stdout:

- a missing file,
- the wrong number of arguments,
- a header that doesn't name the required columns,
- an I/O error on the reader.

## Safety

- No `unsafe`, and no `unwrap`, `expect` or `panic!` outside test code — the
  binary has no reachable panic of its own.
- Arithmetic is checked for both overflow and precision loss; either one skips
  the row rather than wrapping, rounding or aborting.
- Four direct dependencies — `csv`, `serde`, `rust_decimal`, `thiserror` — for 21
  crates total. `rust_decimal` is pulled in with `default-features = false`,
  which drops most of that tree.

## Efficiency

- **Rows are streamed** one at a time into a single reused buffer, so read-side
  memory doesn't depend on file size — 60 MB and 60 GB cost the same to read.
- **Nothing is collected up front**, and the output is written straight out of
  the accounts map.
- **What does grow is the two maps:** one entry per client, capped at 65,536
  because clients are `u16`, and one per disputable transaction. The second is
  the real cost and I don't think it's avoidable — a dispute can reference any
  earlier transaction by ID, so nothing can be dropped until the input ends.

Two million rows, 59 MB:

```
elapsed:  1.05 s
peak RSS:  156 MB
```

Nearly all of that memory is the transaction map. If it had to come down, in
order:

1. pack the entry into `(u16, i64, u8)` instead of holding a `Decimal`;
2. check whether the business allows a dispute window, so old entries can be
   dropped;
3. only then consider spilling to disk.

### If this were a server

- `csv_io::process` takes any `Read`, `write_accounts` takes any `Write`, and
  `Engine` is a plain owned struct with no globals — so N connections is just N
  engines and no synchronization.
- `process` also takes the handler for skipped rows, so a server sends those to
  its own log instead of a shared stderr. `run` is only the convenience wrapper
  the CLI uses.
- **Many streams sharing one ledger** is the more interesting version.
  Transactions touch exactly one client and clients don't interact, so I'd shard
  by client ID: a fixed pool of workers, each owning a disjoint range of clients
  and its own maps, fed by channels from the connection handlers. Per-client
  ordering is preserved, which is all the spec asks for, and nothing needs a lock
  on the hot path. Reporting balances becomes asking each shard for its slice.

### Why there's no tokio here

- **This program never waits.** Reading the 59 MB benchmark file with `cat` takes
  under 10 ms against the ~1.05 s the engine spends on it, so more than 99% of
  the time is CPU — splitting fields, parsing decimals, hashing keys.
- **Async makes waiting cheap; it doesn't make arithmetic faster.**
- **File I/O has no real async path on Linux outside `io_uring`**, so `tokio::fs`
  would hand the reads to a threadpool and pay thread handoffs to reach the same
  `read(2)`.
- **Where it would earn its place** is the connection layer of the server above,
  where thousands of mostly-idle sockets are exactly what a reactor is for. The
  engine underneath would stay synchronous and get called from a worker per
  shard. Nothing in the current code stands in the way of that, which is the
  actual reason for keeping the engine transport-agnostic.
