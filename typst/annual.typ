#import "lib.typ": *

#show: report.with(title: "Annual Financial Report", subtitle: "Fiscal year " + data.fy_label)

#text(fill: muted)[#data.period. Presented at the Annual General Meeting.]

#v(4pt)
#figures(data.figures)

= Summary of funds

#fin-table(
  columns: (2.4fr, 1fr, 1fr, 1fr),
  align: "lrrr",
  header: ("", "General fund", "Gaming fund", "Total"),
  rows: data.summary,
)

#for f in data.funds [
  = #f.title: budget and actual
  #fin-table(
    columns: (2.8fr, 1fr, 1fr, 1.1fr),
    align: "lrrr",
    header: ("Line item", "Budget", "Actual", "Better / (worse)"),
    rows: f.rows,
  )
]

= Bank accounts at #data.year_end

#fin-table(
  columns: (2.4fr, 1fr, 1.4fr, 1fr),
  align: "lrrr",
  header: ("Account", "Book balance", "Bank balance", "Not yet cleared"),
  rows: data.accounts,
)

= Income and spending by month

#fin-table(
  columns: (1.4fr, 1fr, 1fr, 1fr, 1fr),
  align: "lrrrr",
  header: ("Month", "General in", "General out", "Gaming in", "Gaming out"),
  rows: data.months,
  size: 9pt,
)

#if data.gaming != none [
  = BC Community Gaming Grant
  #fin-table(
    columns: (3fr, 1fr),
    align: "lr",
    header: ("Gaming Account Summary", "Amount"),
    rows: data.gaming.rows,
  )
  #for c in data.gaming.checks [
    #callout(c.level, c.message)
  ]
]

#if data.notes != "" [
  = Treasurer's notes
  #data.notes
]

#if data.flags.len() > 0 [
  = Items to resolve
  #for f in data.flags [
    #callout("warning", f)
  ]
]

#v(16pt)
#block(breakable: false)[
  = Approval
  These statements were presented to the members at the Annual General Meeting.
  #v(4pt)
  #signatures("Treasurer" + if data.treasurer != "" { " — " + data.treasurer } else { "" }, "Chair" + if data.chair != "" { " — " + data.chair } else { "" }, "Date")
]
