-- Every tracked download looks its history up by download id on each
-- queue refresh; without an index that is a table scan per item.
CREATE INDEX history_download_id ON history (download_id);
