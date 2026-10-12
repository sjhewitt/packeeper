//! Rows as they come out of SQLite.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::money::Money;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Fund {
    General,
    Gaming,
}

impl Fund {
    pub const ALL: [Fund; 2] = [Fund::General, Fund::Gaming];

    pub fn as_str(self) -> &'static str {
        match self {
            Fund::General => "general",
            Fund::Gaming => "gaming",
        }
    }

    pub fn parse(s: &str) -> Option<Fund> {
        match s {
            "general" => Some(Fund::General),
            "gaming" => Some(Fund::Gaming),
            _ => None,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Fund::General => "General Fund",
            Fund::Gaming => "Gaming Grant Fund",
        }
    }
}

impl std::fmt::Display for Fund {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct Account {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub institution: String,
    pub account_number: String,
    pub opening_date: NaiveDate,
    pub opening_balance_cents: i64,
    pub csv_mapping: Option<String>,
    pub active: bool,
    pub notes: String,
}

impl Account {
    pub fn fund(&self) -> Fund {
        Fund::parse(&self.kind).unwrap_or(Fund::General)
    }

    pub fn is_gaming(&self) -> bool {
        self.fund() == Fund::Gaming
    }

    pub fn opening_balance(&self) -> Money {
        Money(self.opening_balance_cents)
    }
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct FiscalYear {
    pub id: i64,
    pub label: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub budget_status: String,
    pub budget_approved_on: Option<NaiveDate>,
    pub notes: String,
}

impl FiscalYear {
    pub fn contains(&self, date: NaiveDate) -> bool {
        self.start_date <= date && date <= self.end_date
    }

    pub fn period(&self) -> String {
        format!("{} to {}", self.start_date.format("%B %-d, %Y"), self.end_date.format("%B %-d, %Y"))
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct LineItem {
    pub id: i64,
    pub name: String,
    pub fund: String,
    pub kind: String,
    pub gasr_line: Option<String>,
    pub receipt_required: bool,
    pub description: String,
    pub sort_order: i64,
    pub active: bool,
}

impl LineItem {
    pub fn fund(&self) -> Fund {
        Fund::parse(&self.fund).unwrap_or(Fund::General)
    }

    /// "Gaming: Field trips" style label for selects that mix funds.
    pub fn label(&self) -> String {
        match self.kind.as_str() {
            "income" => format!("{} (income)", self.name),
            "transfer" => format!("{} (transfer)", self.name),
            _ => self.name.clone(),
        }
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct Transaction {
    pub id: i64,
    pub account_id: i64,
    pub date: NaiveDate,
    pub description: String,
    pub payee: String,
    pub amount_cents: i64,
    pub reference: String,
    pub purpose: String,
    pub bank_balance_cents: Option<i64>,
    pub source: String,
    pub import_batch_id: Option<i64>,
    #[allow(dead_code)] // matched in SQL only
    pub bank_ref: Option<String>,
    pub cleared: bool,
    pub reimbursed_by_id: Option<i64>,
    pub notes: String,
    pub created_at: String,
}

impl Transaction {
    pub fn amount(&self) -> Money {
        Money(self.amount_cents)
    }

    pub fn bank_balance(&self) -> Option<Money> {
        self.bank_balance_cents.map(Money)
    }

    /// The payee if the treasurer set one, else the bank description.
    pub fn display_name(&self) -> &str {
        if self.payee.is_empty() { &self.description } else { &self.payee }
    }
}

#[derive(Debug, Clone, FromRow)]
#[allow(dead_code)] // id and transaction_id are row columns, used in SQL
pub struct Allocation {
    pub id: i64,
    pub transaction_id: i64,
    pub line_item_id: i64,
    pub amount_cents: i64,
    pub memo: String,
}

impl Allocation {
    pub fn amount(&self) -> Money {
        Money(self.amount_cents)
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct Attachment {
    pub id: i64,
    pub transaction_id: i64,
    pub filename: String,
    pub stored_name: String,
    pub content_type: String,
    pub size_bytes: i64,
    #[allow(dead_code)] // kept to detect changed or duplicate files
    pub sha256: String,
    pub uploaded_at: String,
}

impl Attachment {
    pub fn is_image(&self) -> bool {
        self.content_type.starts_with("image/")
    }

    pub fn size_label(&self) -> String {
        let b = self.size_bytes as f64;
        if b >= 1_048_576.0 { format!("{:.1} MB", b / 1_048_576.0) } else { format!("{:.0} KB", (b / 1024.0).max(1.0)) }
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct ImportBatch {
    pub id: i64,
    pub account_id: i64,
    pub filename: String,
    pub stored_name: String,
    pub format: String,
    pub status: String,
    pub created_at: String,
    pub rows_total: i64,
    pub rows_new: i64,
    pub rows_duplicate: i64,
    pub rows_matched: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct Rule {
    pub id: i64,
    pub pattern: String,
    pub account_id: Option<i64>,
    pub line_item_id: i64,
    pub payee: String,
}

#[derive(Debug, Clone, FromRow)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub display_name: String,
    pub password_hash: String,
    pub role: String,
    #[allow(dead_code)]
    pub created_at: String,
}

impl User {
    pub fn can_edit(&self) -> bool {
        self.role == "treasurer"
    }
}
