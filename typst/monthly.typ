#import "lib.typ": *

#show: report.with(title: "Treasurer's Report", subtitle: data.month_label + " · Fiscal year " + data.fy_label)

#figures(data.figures)

= Bank accounts

#fin-table(
  columns: (2.2fr, 1fr, 1fr, 1fr, 1fr, 1.4fr),
  align: "lrrrrr",
  header: ("Account", "Opening", "Money in", "Money out", "Closing", "Bank balance"),
  rows: data.accounts,
)

#for o in data.outstanding [
  #callout("info")[Not yet cleared at the bank: #o]
]
#for w in data.reconcile_warnings [
  #callout("warning", w)
]

#for f in data.funds [
  = #f.title
  #fin-table(
    columns: (2.6fr, 1fr, 1fr, 1fr, 1fr),
    align: "lrrrr",
    header: ("Line item", "Annual budget", data.month_short, "Year to date", "Remaining"),
    rows: f.rows,
  )
  #text(9pt, fill: muted)[
    Fund balance: #f.opening at the start of the month, #f.closing at the end.
    #if f.transfers != "" [Transfers between accounts: #f.transfers.]
  ]
]

#if data.fundraisers.len() > 0 [
  = Fundraisers
  #fin-table(
    columns: (2.2fr, 1fr, 1fr, 1fr, 1fr, 1fr),
    align: "lrrrrr",
    header: ("Fundraiser", "Net " + data.month_short, "Income to date", "Costs to date", "Net to date", "Goal"),
    rows: data.fundraisers,
  )
  #text(9pt, fill: muted)[Net is income less costs. The same money is also in the line items above.]
]

= Transactions in #data.month_label

#if data.transactions.len() == 0 [
  #text(fill: muted)[No transactions this month.]
] else [
  #fin-table(
    columns: (0.65fr, 1.35fr, 2.5fr, 1.85fr, 0.95fr, 0.45fr),
    align: "llllrc",
    header: ("Date", "Account", "Description", "Line item", "Amount", "Rcpt"),
    rows: data.transactions,
    size: 8.5pt,
  )
]

#if data.flags.len() > 0 [
  = Items to resolve
  #for f in data.flags [
    #callout("warning", f)
  ]
]
