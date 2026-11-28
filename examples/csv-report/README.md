# csv-report

A small data-report CLI: reads a sales CSV, checks every row, and prints
revenue by region, by month and by product, plus statistics of order
value. `--json` writes the same report as JSON.

It shows:

- `csv`: typed rows with `#[derive(csv::Csv)]`, read from a file stream
  one record at a time; decode errors carry the line and field.
- `table`: rows to a table with `#[derive(table::Record)]`, a computed
  column (`col("units") .* col("price")`), group-by, aggregates, sorting.
- `stats`: `describe` and `quantile(90pct)` on the value column.
- `json`: the report as JSON with `#[derive(json::Json)]`.
- `throws` / `try` / `catch` with typed errors (`throws (str)`,
  `throws (csv::CsvError | str)`), `let .. else`, `T | none` with `??`,
  closures and iterator chains, `|>`.

## Run

```sh
nabi run -- data/sales.csv
nabi run -- data/sales.csv --json
```

`data/sales.csv` holds 200 orders (`date,region,product,units,price`).
A row that does not validate is reported on stderr as `file:line:` and
left out; a file that cannot be read at all (missing, unclosed quote,
missing column) exits with status 1, bad arguments with 2.

## Sample output

```text
$ nabi run -- data/sales.csv
data/sales.csv: 200 orders, 0 rejected

Revenue by region
region  orders  units    revenue  share
------  ------  -----  ---------  -----
West        56    350  31,607.49  33.9%
North       64    266  30,209.89  32.4%
South       48    242  18,272.87  19.6%
East        32    169  13,028.53  14.0%

Revenue by month
month    orders  units    revenue  share
-------  ------  -----  ---------  -----
2026-01      39    161  18,161.94  19.5%
2026-02      28    168  12,342.65  13.3%
2026-03      31    202  17,970.75  19.3%
2026-04      41    166  11,575.14  12.4%
2026-05      33    181  14,028.00  15.1%
2026-06      28    149  19,040.30  20.4%

Top products
product        orders  units    revenue  share
-------------  ------  -----  ---------  -----
Standing Desk      10     46  19,980.50  21.5%
USB-C Dock         18    137  16,776.45  18.0%
Office Chair       14     90  16,405.20  17.6%
Headset            15     96  11,316.90  12.2%
Keyboard           21     85   7,443.86   8.0%

Order value
statistic            value
---------------  ---------
total            93,118.78
mean                465.59
median              214.70
std dev             658.15
90th percentile   1,216.02
min                  13.05
max               4,644.00
```

Bad rows, on stderr:

```text
broken.csv:4: skipped: field `units`: `four` is not a whole number that fits in i64; write one such as `42`
broken.csv:11: skipped: `2026-13-05` is not a date; write YYYY-MM-DD
broken.csv:21: skipped: unknown region `Central`; use one of North, South, East, West
broken.csv:41: skipped: 4 fields, the header has 5
```
