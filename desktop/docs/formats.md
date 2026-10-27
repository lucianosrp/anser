# Format definitions: teaching finance a new bank

Most bank exports are a table: a date, an amount, a description, maybe a
balance. finance reads those without new code. A small TOML file, a **format
definition**, says which column is which and how the cells are written.
Installing support for a bank ("the Barclays extension") means dropping that
file into a formats folder.

The code sources (N26, HSBC, Revolut, Mox, Trezor, the HSBC SMS backup) are Rust
modules in `engine/src/sources/` and are always tried first. A definition
only gets a file none of them claims. Compiled code plugins for exports a
table can't describe (PDF statements, crypto ledgers) may come later. For
now those still need a Rust module.

## Where definitions live

| Where | What |
|---|---|
| `engine/formats/*.toml` | built in, compiled into the engine: `monzo.toml`, `barclays_uk.toml` |
| `$FINANCE_HOME/formats/*.toml` on the server (`$FINANCE_FORMATS_DIR` overrides it) | what the server's ingest uses. `PUT /api/formats/{id}` writes here |
| `~/.config/finance-desktop/formats/*.toml` (`$FINANCE_DESKTOP_FORMATS`) | the desktop app's own, for the Upload page's preview. The column mapper saves here |
| `<desktop cache>/formats/` | the desktop app's copy of the server's definitions (`GET /api/formats`, pulled when the app starts) |

When two files share an `id`, the first one wins in this order: the user's
folders, then the built-in ones. To change a built-in definition, copy it
into your formats folder and edit the copy. The folders are re-read when a
file in them is added, changed or removed (checked at most every 2 seconds),
so nothing needs a restart.

On the server, a file in `transactions/` that nothing could read stays
there. Once a definition that reads it arrives, the next ingest picks it up.
`PUT /api/formats/{id}` runs that ingest right away.

## The quickest way: Map columns…

Drop the export on the desktop app's Upload page. If no source or definition
reads it, its row says *unknown format* and offers **Map columns…**. That
dialog shows the first rows. You pick the date, amount (or money out and
money in) and description columns, plus the balance, account or currency
columns if the file has them. The date pattern is detected and can be
overridden. You also set the decimal separator, the sign convention and the
kind (bank, card, wallet). A live preview shows the rows as the definition
reads them, with every unreadable row in red.

**Save as format** names the definition and writes `<id>.toml` to this
machine's formats folder. It then sends the definition to the server
(`PUT /api/formats/{id}`) and reads the file again, so the file previews and
uploads like any other source. You can edit the saved file by hand later.

## Writing one by hand

```toml
id = "barclays_uk"           # the source key rows are stored under: a-z, 0-9, _
label = "Barclays"           # display name
institution = "Barclays Bank UK"   # optional
country = "GB"               # optional
kind = "bank"                # bank | card | wallet (card: no balance, out of net worth)
currency = "GBP"             # or columns.currency for a multi-currency export
balance_method = "cumsum"    # reported | cumsum | none; default: none for a card,
                             # reported with a balance column, else cumsum
cashflow = false             # part of the overview's monthly cash flow
dedup = []                   # columns that tell identical rows apart (a transaction id)

[detect]
columns = ["Number", "Date", "Account", "Amount", "Subcategory", "Memo"]
filename = "*.csv"           # optional glob, any case

[columns]
date = "Date"
amount = "Amount"            # one signed column, or:
# debit = "Paid out"         #   money out ...
# credit = "Paid in"         #   ... and money in (both read as positive numbers)
description = "Memo"         # or a list, joined with a space: ["Payee", "Reference"]
account = "Account"          # optional
# balance = "Balance"        # optional
# currency = "Currency"      # optional, per row
# direction = "Af Bij"       # optional debit/credit marker column, with parse.debit_markers

[parse]
date_format = "%d/%m/%Y"     # chrono strftime; a list is tried in order; ISO always works
decimal = "."                # "." or ","
# thousands = ","            # default: "," for decimal ".", "." for decimal ","; "" for none
# sign = "inverted"          # positive = money out (many card exports); default "signed"
# debit_markers = ["Af", "D"]
# skip_rows = 3              # rows above the header; default: found by the detect columns
# encoding = "latin-1"       # utf-8 (default), latin-1, windows-1252, utf-16
# delimiter = ";"            # default: sniffed (, ; tab |); "\t" for tab
# sheet = "Transactions"     # Excel: the sheet; default the first
```

How a file is read:

- **Detection.** The file is a `.csv`, `.txt`, `.tsv`, `.xlsx`, `.xls` or
  `.xlsm`, it matches `detect.filename` if one is set, and one of its first
  30 rows has every `detect.columns` name. Case and surrounding spaces are
  ignored. That row is the header, so a preamble above it is fine.
- **Rows.** A blank row, or a row without a date and without an amount (a
  footer such as "Closing balance"), is skipped. Any other row that can't
  be read fails the file with its line number. A partial import never
  happens.
- **Numbers.** Currency signs and codes around a number are ignored.
  `(12.50)`, `12.50-` and `12.50 DR` are negative. Excel numbers are taken
  as they are, whatever `decimal` says.
- **Dates.** Excel date cells are read directly. A time after a date-only
  pattern is ignored.
- **Identity.** Rows are stored with `id` as their source and hash like any
  code source's rows (`ingest::natural_key`). Re-importing a file adds
  nothing, and an overlapping export adds only its new rows. Without a
  balance column, two identical rows on one day would collapse into one.
  List a transaction-id column in `dedup` to keep them apart. The Monzo
  example does this.

A definition can't reuse a code source's key (`n26`, `hsbc`, ...). Unknown
keys are refused, so a typo is an error rather than silently ignored.
`GET /api/formats` lists the definitions in effect and any file that failed
to load, with the reason.

## Describing the export: `[export]`

Optional. It tells people how to get this bank's file, and shows on the
Supported sources page and in the setup wizard:

```toml
[export]
format = "CSV"                       # what the file is
how = ["Sign in…", "Open the account…", "Choose Export, as CSV."]  # the steps
docs_url = "https://…"               # optional, the bank's own help page
notes = "One file per account."      # optional, one line
```

`format` and at least one step in `how` are required when the table is there.
The two built-in definitions have one; the catalog's generic line shows for a
definition without it.

## Examples

Two definitions ship built in. Each has a synthetic sample export (made-up
rows in the bank's layout) in `docs/formats/samples/`:

- [`engine/formats/monzo.toml`](../engine/formats/monzo.toml) and
  [`samples/monzo-sample.csv`](formats/samples/monzo-sample.csv): a signed
  amount, no balance (cumsum), and `dedup = ["Transaction ID"]`, so the two
  identical coffees in the sample stay two rows.
- [`engine/formats/barclays_uk.toml`](../engine/formats/barclays_uk.toml) and
  [`samples/barclays_uk-sample.csv`](formats/samples/barclays_uk-sample.csv):
  an account column and a free-text memo.

Both are modeled on the banks' export layouts. If your real export differs,
copy the file into your formats folder and fix it.

`engine/tests/formats.rs` covers more: a semicolon-separated export with a
preamble, decimal commas and a two-column description; an `.xlsx` card
statement with debit and credit columns on its second sheet; and the
ingest, re-ingest and overlap guarantees.
