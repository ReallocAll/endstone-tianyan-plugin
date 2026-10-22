#include "database.hpp"
#include <cassert>
#include <filesystem>
int main() {
    // Failed initialization has no possible returning lease: fail, never wait forever.
    bool failed = false;
    try {
        yuhangle::ConnectionPool pool("/nonexistent-tianyan-test-parent/database.sqlite", 1);
        pool.getConnection();
    } catch (const std::runtime_error&) { failed = true; }
    assert(failed);
    auto path = std::filesystem::temp_directory_path() / ("tianyan-pool-" + std::to_string(std::chrono::steady_clock::now().time_since_epoch().count()) + ".sqlite");
    {
        yuhangle::Database db(path.string());
        // Repeated prepare failures used to exhaust all five pooled connections.
        for (int i = 0; i < 12; ++i) assert(!db.updateStatusesByUUIDs({{"a", "reverted"}}));
        assert(db.init_database() == SQLITE_OK);
        assert(db.addLog("a", "player", "Alice", 0, 0, 0, "Overworld", "stone", "Stone", 1, "block_break", "", "") == SQLITE_OK);
        assert(db.addLog("b", "player", "Bob", 0, 0, 0, "Overworld", "stone", "Stone", 1, "block_break", "", "") == SQLITE_OK);
        assert(db.executeSQL("CREATE TRIGGER reject_status BEFORE UPDATE ON LOGDATA WHEN new.status = 'failure' BEGIN SELECT RAISE(ABORT, 'injected'); END") == SQLITE_OK);
        assert(!db.updateStatusesByUUIDs({{"a", "reverted"}, {"b", "failure"}}));
        std::vector<std::map<std::string, std::string>> rows;
        assert(db.querySQL("SELECT status FROM LOGDATA WHERE uuid='a'", rows) == SQLITE_OK);
        assert(rows.size() == 1 && rows[0]["status"].empty());
        assert(db.updateStatusesByUUIDs({{"a", "reverted"}}));
        // Duplicate replay is harmless under the existing UUID primary key.
        assert(db.addLog("a", "player", "Alice", 0, 0, 0, "Overworld", "stone", "Stone", 1, "block_break", "", "") == SQLITE_OK);
        rows.clear(); assert(db.querySQL("SELECT COUNT(*) AS count FROM LOGDATA", rows) == SQLITE_OK);
        assert(rows[0]["count"] == "2");
    }
    std::filesystem::remove(path);
    yuhangle::ConnectionPool pool(":memory:", 1);
    auto connection = pool.getConnection();
    assert(connection && connection->get());
    auto* identity = connection.get();
    // Scope release also rolls back an interrupted transaction before reuse.
    assert(sqlite3_exec(connection->get(), "BEGIN; CREATE TABLE abandoned (id INTEGER);", nullptr, nullptr, nullptr) == SQLITE_OK);
    connection.reset();
    auto reused = pool.getConnection();
    assert(reused.get() == identity);
    assert(sqlite3_get_autocommit(reused->get()));
    assert(sqlite3_exec(reused->get(), "SELECT * FROM abandoned", nullptr, nullptr, nullptr) != SQLITE_OK);
}
