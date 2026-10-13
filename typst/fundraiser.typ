#import "lib.typ": *

#show: report.with(title: "Fundraiser Report", subtitle: data.name + " · Fiscal year " + data.fy_label)

#text(fill: muted)[#data.period.#if data.description != "" [ #data.description]]

#v(4pt)
#figures(data.figures)

#for c in data.checks [
  #callout("warning", c)
]

= By line item

#if data.by_line.len() == 0 [
  #text(fill: muted)[No transactions in this fiscal year.]
] else [
  #fin-table(
    columns: (3fr, 1fr),
    align: "lr",
    header: ("Line item", "Amount"),
    rows: data.by_line,
  )
]

#if data.transactions.len() > 0 [
  = Transactions
  #fin-table(
    columns: (0.65fr, 3.2fr, 2fr, 1fr, 0.45fr),
    align: "lllrc",
    header: ("Date", "Description", "Line item", "Amount", "Rcpt"),
    rows: data.transactions,
    size: 8.5pt,
  )
]

= Year by year

#if data.history.len() == 0 [
  #text(fill: muted)[No years with transactions or goals yet.]
] else [
  #fin-table(
    columns: (1.6fr, 1fr, 1fr, 1fr, 1fr),
    align: "lrrrr",
    header: ("Fiscal year", "Income", "Costs", "Net", "Goal"),
    rows: data.history,
  )
]

#v(6pt)
#text(9pt, fill: muted)[Income is the money in and costs are the money out, of the transactions or parts of transactions that belong to this fundraiser. Net is income less costs.]
