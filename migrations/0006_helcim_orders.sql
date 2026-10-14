-- Card orders paid through the PAC's own Helcim account. Each checkout gets a
-- new invoice number; the payment with that number pays the order.
ALTER TABLE lunch_orders ADD COLUMN helcim_invoice TEXT;
ALTER TABLE lunch_orders ADD COLUMN helcim_transaction TEXT;
CREATE UNIQUE INDEX lunch_orders_helcim_invoice ON lunch_orders (helcim_invoice);
