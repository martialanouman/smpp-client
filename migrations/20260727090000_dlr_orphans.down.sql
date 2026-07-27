-- Reverses 20260727090000_dlr_orphans.up.sql (guide §11.2).
--
-- Dropping the table loses every orphaned receipt recorded so far. There is no
-- other home for them — the whole point of the table is that these receipts
-- belong to no message — so a downgrade is a deliberate loss of diagnostic
-- data, not a reversible move. The indexes go with the table.

DROP TABLE dlr_orphans;
