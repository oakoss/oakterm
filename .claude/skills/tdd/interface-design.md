# Interface Design for Testability

1. **Accept dependencies, don't create them**

   ```rust
   // Testable: caller controls the database
   fn load_default_metrics(db: &fontdb::Database, size: f32) -> io::Result<FontMetrics>

   // Hard to test: creates database internally
   fn load_default_metrics(size: f32) -> io::Result<FontMetrics> {
       let mut db = fontdb::Database::new();
       db.load_system_fonts(); // slow, non-deterministic
   }
   ```

2. **Return results, don't produce side effects**

   ```rust
   // Testable: returns data
   fn dirty_rows(&self, since_seqno: u64) -> Vec<u16>

   // Hard to test: mutates external state
   fn mark_rows_clean(&mut self)
   ```

3. **Small surface area** — fewer methods = fewer tests needed
