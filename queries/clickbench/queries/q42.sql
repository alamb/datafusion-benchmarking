
SELECT DATE_TRUNC('minute', to_timestamp_seconds("EventTime")) AS M, COUNT(*) AS PageViews FROM hits WHERE "CounterID" = 62 AND "EventDate" >= '2013-07-14' AND "EventDate" <= '2013-07-15' AND "IsRefresh" = 0 AND "DontCountHits" = 0 GROUP BY DATE_TRUNC('minute', to_timestamp_seconds("EventTime")) ORDER BY DATE_TRUNC('minute', M) LIMIT 10 OFFSET 1000;
