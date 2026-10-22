#pragma once
#include <mutex>
#include <string>
#include <unordered_set>

// Tracks world mutations separately from their asynchronous database acknowledgement.
// A failed status write must never make a successful summon/setblock eligible again.
class RevertGuard {
public:
    bool contains(const std::string& uuid) const {
        std::lock_guard lock(mutex_);
        return applied_.contains(uuid);
    }
    void record(const std::string& uuid) {
        std::lock_guard lock(mutex_);
        applied_.insert(uuid);
        pending_.insert(uuid);
    }
    void acknowledge(const std::string& uuid) {
        std::lock_guard lock(mutex_);
        pending_.erase(uuid);
    }
    // Call only when no rollback query/result remains which could contain stale rows.
    void forgetSettled() {
        std::lock_guard lock(mutex_);
        std::erase_if(applied_, [this](const auto& uuid) { return !pending_.contains(uuid); });
    }
private:
    mutable std::mutex mutex_;
    std::unordered_set<std::string> applied_, pending_;
};
