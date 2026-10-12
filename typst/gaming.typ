#import "lib.typ": *

#show: report.with(title: "Gaming Account Summary", subtitle: "Fiscal year ending " + data.year_end)

#text(fill: muted)[
  Worksheet for the BC _Gaming Account Summary Report_.
  Copy these figures onto the official form.
  #if data.file_number != "" [L&G file number: #data.file_number.]
  Due by #data.due_date.
]

#for c in data.checks.filter(c => c.level != "info") [
  #callout(c.level, c.message)
]

= Section 3: gaming revenue

#fin-table(
  columns: (0.55fr, 3.2fr, 1fr),
  align: "llr",
  header: ("Line", "Description", "Amount"),
  rows: data.revenue,
)

== Lines 6 to 13: gross revenue from gaming event licences

#fin-table(
  columns: (0.45fr, 1fr, 1.5fr, 0.85fr, 0.85fr, 0.9fr),
  align: "lllllr",
  header: ("Line", "Licence number", "Licence type", "From", "To", "Amount"),
  rows: data.licences,
  size: 9pt,
)
#text(8.5pt, fill: muted)[The reporting period is the licensed activity that falls within this fiscal year. Report all revenue received in the year.]

= Section 4: prize costs and expenses for licensed gaming events

#fin-table(
  columns: (0.55fr, 3.2fr, 1fr),
  align: "llr",
  header: ("Line", "Description", "Amount"),
  rows: data.costs,
)

#if data.event_costs.len() > 0 [
  == Payments behind lines 19 to 26
  #fin-table(
    columns: (0.85fr, 0.75fr, 1.7fr, 1fr, 0.75fr, 0.9fr),
    align: "lllllr",
    header: ("Date", "Ref #", "Paid to", "Licence", "Kind", "Amount"),
    rows: data.event_costs,
    size: 8.5pt,
  )
]

= Sections 2, 5, and 6: balances

#fin-table(
  columns: (0.55fr, 3.2fr, 1fr),
  align: "llr",
  header: ("Line", "Description", "Amount"),
  rows: data.proceeds,
)

= Section 5: gaming fund disbursements (lines 30 to 45)

#if data.disbursements.len() == 0 [
  #text(fill: muted)[No disbursements in this fiscal year.]
] else [
  #fin-table(
    columns: (0.85fr, 0.9fr, 1.6fr, 2.4fr, 0.9fr),
    align: "llllr",
    header: ("Date", "Cheque / EFT / Ref #", "Disbursed to", "Purpose", "Amount"),
    rows: data.disbursements,
    size: 8.5pt,
  )
]

#for r in data.reimbursements [
  == Reimbursement summary: #r.title
  #fin-table(
    columns: (0.85fr, 1.7fr, 2.6fr, 0.9fr),
    align: "lllr",
    header: ("Date", "Disbursed to", "Purpose", "Amount"),
    rows: r.rows,
    size: 8.5pt,
  )
]

= Section 7: bank balances at year end

#fin-table(
  columns: (3fr, 1fr),
  align: "lr",
  header: ("Account", "Balance"),
  rows: data.accounts,
)

= Deposits

#fin-table(
  columns: (0.85fr, 2.6fr, 0.7fr, 1fr),
  align: "llcr",
  header: ("Date", "Description", "Line", "Amount"),
  rows: data.deposits,
  size: 8.5pt,
)

#if data.grants.len() > 0 [
  = Grant spending deadlines
  #text(9pt, fill: muted)[PAC grants must be spent within 24 months of receipt. The year-end balance is assigned to the newest money in first.]
  #fin-table(
    columns: (1fr, 1fr, 1fr, 1fr),
    align: "lrlr",
    header: ("Received", "Amount", "Spend by", "Unspent at year end"),
    rows: data.grants,
  )
]

#for c in data.checks.filter(c => c.level == "info") [
  #callout(c.level, c.message)
]

#v(16pt)
#block(breakable: false)[
  = Section 10: certification
  Two unrelated executive members sign the official form, one of them the treasurer. Electronic signatures are not accepted.
  #v(4pt)
  #signatures("Treasurer", "Executive member", "Prepared by")
]
