use chrono::NaiveDate;
use sqlx::SqlitePool;

use super::*;

fn d(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

async fn pool() -> (SqlitePool, std::path::PathBuf) {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "pac-lunch-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let pool = crate::db::open(&dir).await.unwrap();
    crate::db::seed(&pool).await.unwrap();
    (pool, dir)
}

async fn exec(pool: &SqlitePool, sql: &str) -> i64 {
    sqlx::query(sqlx::AssertSqlSafe(sql.to_string())).execute(pool).await.unwrap().last_insert_rowid()
}

/// Two items, one division, two children for parent 7 and one for parent 8,
/// and an open period with Monday and Wednesday lunches in the week of Jan 4, 2027.
async fn setup(pool: &SqlitePool) -> (Period, i64, i64, i64, i64) {
    let pizza = exec(pool, "INSERT INTO lunch_items (name, price_cents) VALUES ('Pizza', 550)").await;
    let sushi = exec(pool, "INSERT INTO lunch_items (name, price_cents) VALUES ('Sushi', 700)").await;
    let div = exec(pool, "INSERT INTO lunch_divisions (name, sort_order) VALUES ('Div 3', 3)").await;
    for (person, name) in [(7, "Ava"), (7, "Ben"), (8, "Cal")] {
        sqlx::query("INSERT INTO lunch_children (person_id, name, division_id, created_at) VALUES (?, ?, ?, '')")
            .bind(person)
            .bind(name)
            .bind(div)
            .execute(pool)
            .await
            .unwrap();
    }
    let id = exec(
        pool,
        "INSERT INTO lunch_periods (name, order_deadline, status, created_at) VALUES ('January', '2027-01-02', 'open', '')",
    )
    .await;
    // Monday Jan 4 to Friday Jan 8, on Mondays and Wednesdays: pizza both days.
    assert_eq!(add_days(pool, id, d("2027-01-04"), d("2027-01-08"), &[1, 3], &[pizza]).await.unwrap(), 2);
    let days = days(pool, id).await.unwrap();
    // Sushi on Wednesday too.
    set_day_items(pool, days[1].0.id, &[pizza, sushi]).await.unwrap();
    (period(pool, id).await.unwrap().unwrap(), pizza, sushi, days[0].0.id, days[1].0.id)
}

fn kid(children: &[Child], name: &str) -> i64 {
    children.iter().find(|c| c.name == name).unwrap().id
}

#[tokio::test]
async fn orders_check_children_menu_and_deadline() {
    let (pool, dir) = pool().await;
    let (period, pizza, sushi, monday, wednesday) = setup(&pool).await;
    let mine = children(&pool, 7).await.unwrap();
    let (ava, ben) = (kid(&mine, "Ava"), kid(&mine, "Ben"));
    let cal = children(&pool, 8).await.unwrap()[0].id;
    let today = d("2027-01-01");
    let order = |choices: Vec<Choice>, method: &'static str, when: NaiveDate| {
        let (pool, period) = (pool.clone(), period.clone());
        async move { create_order(&pool, 7, &period, method, &choices, when).await.unwrap() }
    };
    let c = |child_id, day_id, item_id| Choice { child_id, day_id, item_id, quantity: 1 };

    assert!(order(vec![], "card", today).await.is_err(), "nothing chosen");
    assert!(order(vec![c(cal, monday, pizza)], "card", today).await.is_err(), "another parent's child");
    assert!(order(vec![c(ava, monday, sushi)], "card", today).await.is_err(), "sushi is not on Monday");
    assert!(order(vec![c(ava, monday, pizza)], "cheque", today).await.is_err(), "unknown method");
    assert!(order(vec![c(ava, monday, pizza)], "card", d("2027-01-03")).await.is_err(), "after the deadline");

    let o = order(vec![c(ava, monday, pizza), c(ava, wednesday, sushi), c(ben, wednesday, pizza)], "etransfer", today)
        .await
        .unwrap();
    assert_eq!(o.total_cents, 550 + 700 + 550);
    assert_eq!((o.status.as_str(), o.method.as_str()), ("pending", "etransfer"));

    // A later price change does not change the order or the days already set up.
    exec(&pool, &format!("UPDATE lunch_items SET price_cents = 600 WHERE id = {pizza}")).await;
    let lines = order_lines(&pool, o.id).await.unwrap();
    assert_eq!(lines.iter().map(|l| l.price_cents).sum::<i64>(), 1800);
    let again = order(vec![c(ava, monday, pizza)], "card", today).await.unwrap();
    assert_eq!(again.total_cents, 550, "the day keeps its price");

    // A draft period takes no orders.
    exec(&pool, &format!("UPDATE lunch_periods SET status = 'draft' WHERE id = {}", period.id)).await;
    let draft = super::period(&pool, period.id).await.unwrap().unwrap();
    assert!(create_order(&pool, 7, &draft, "card", &[c(ava, monday, pizza)], today).await.unwrap().is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn counts_payments_and_month_fee() {
    let (pool, dir) = pool().await;
    let (period, pizza, sushi, monday, wednesday) = setup(&pool).await;
    let mine = children(&pool, 7).await.unwrap();
    let ava = kid(&mine, "Ava");
    let today = d("2027-01-01");
    let c = |day_id, item_id| Choice { child_id: ava, day_id, item_id, quantity: 1 };
    let cash = create_order(&pool, 7, &period, "cash", &[c(monday, pizza)], today).await.unwrap().unwrap();
    let card = create_order(&pool, 7, &period, "card", &[c(wednesday, sushi)], today).await.unwrap().unwrap();

    // A card order counts only once paid; a cash order counts while it waits.
    let counts = day_counts(&pool, period.id).await.unwrap();
    assert_eq!(counts[0].items, vec![("Pizza".to_string(), 1)]);
    assert!(counts[1].items.is_empty());
    assert_eq!(counts[0].divisions[0].0, "Div 3");
    assert_eq!(day_sheet(&pool, monday).await.unwrap().len(), 1);
    assert!(day_sheet(&pool, wednesday).await.unwrap().is_empty());

    assert!(monthly_fee_due(&pool, today).await.unwrap());
    set_checkout(&pool, card.id, "cs_1", 254, true).await.unwrap();
    assert!(order_by_session(&pool, "cs_1").await.unwrap().is_some());
    assert!(mark_paid(&pool, card.id, Some("pi_1")).await.unwrap());
    assert!(!mark_paid(&pool, card.id, Some("pi_1")).await.unwrap(), "only once");
    let paid_month = chrono::Local::now().date_naive();
    assert!(!monthly_fee_due(&pool, paid_month).await.unwrap(), "this month's account fee is paid");

    let counts = day_counts(&pool, period.id).await.unwrap();
    assert_eq!(counts[1].items, vec![("Sushi".to_string(), 1)]);
    let money = period_money(&orders_for_period(&pool, period.id).await.unwrap());
    assert_eq!((money.paid, money.fees, money.waiting, money.orders), (Money(700), Money(254), Money(550), 2));
    assert_eq!(money.net(), Money(446));

    // Cancel the cash order; refund the card one.
    assert!(close_order(&pool, cash.id, "cancelled").await.unwrap());
    assert!(!close_order(&pool, cash.id, "refunded").await.unwrap(), "a cancelled order was never paid");
    assert!(close_order(&pool, card.id, "refunded").await.unwrap());
    assert!(day_counts(&pool, period.id).await.unwrap().iter().all(|c| c.items.is_empty()));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn day_menus_keep_ordered_items() {
    let (pool, dir) = pool().await;
    let (period, pizza, sushi, _, wednesday) = setup(&pool).await;
    let ava = kid(&children(&pool, 7).await.unwrap(), "Ava");
    create_order(
        &pool,
        7,
        &period,
        "cash",
        &[Choice { child_id: ava, day_id: wednesday, item_id: sushi, quantity: 1 }],
        d("2027-01-01"),
    )
    .await
    .unwrap()
    .unwrap();
    // Taking sushi off Wednesday keeps it, because a child ordered it.
    set_day_items(&pool, wednesday, &[pizza]).await.unwrap();
    let menu: Vec<i64> = days(&pool, period.id).await.unwrap()[1].1.iter().map(|m| m.item_id).collect();
    assert!(menu.contains(&sushi) && menu.contains(&pizza));
    let _ = std::fs::remove_dir_all(dir);
}
