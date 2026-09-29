# Deterministic tests

No test sleeps for a fixed time and then asserts.

Wait for observable files, store rows, or process states with a generous bounded
deadline. Use explicit gates to order competing tasks. Scheduled jobs may cross
minute boundaries; assert uniqueness per slot rather than elapsed wall time.
Give every test its own temporary directory, including across test processes.
