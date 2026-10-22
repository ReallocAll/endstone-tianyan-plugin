#pragma once
#include <condition_variable>
#include <deque>
#include <functional>
#include <mutex>
#include <stdexcept>
#include <thread>

// Plugin-owned FIFO. stop() closes admission and joins after all accepted work.
// Never detach: a slow operation delays unload rather than executing unloaded code.
class OwnedWorkQueue {
public:
    OwnedWorkQueue() : thread_([this] { run(); }) {}
    ~OwnedWorkQueue() { stop(); }
    OwnedWorkQueue(const OwnedWorkQueue&) = delete;
    void start() {
        std::lock_guard lock(mutex_);
        if (!thread_.joinable()) { stopping_ = false; thread_ = std::thread([this] { run(); }); }
    }
    void submit(std::function<void()> task) {
        std::lock_guard lock(mutex_);
        if (stopping_) throw std::runtime_error("Plugin worker is stopping");
        if (tasks_.size() >= 256) throw std::runtime_error("Plugin worker queue is full");
        tasks_.push_back(std::move(task));
        ready_.notify_one();
    }
    void waitIdle() {
        std::unique_lock lock(mutex_);
        idle_.wait(lock, [this] { return tasks_.empty() && !active_; });
    }
    void stop() {
        { std::lock_guard lock(mutex_); stopping_ = true; }
        ready_.notify_all();
        if (thread_.joinable()) thread_.join();
    }
private:
    void run() {
        for (;;) {
            std::function<void()> task;
            {
                std::unique_lock lock(mutex_);
                ready_.wait(lock, [this] { return stopping_ || !tasks_.empty(); });
                if (tasks_.empty()) return;
                task = std::move(tasks_.front()); tasks_.pop_front(); active_ = true;
            }
            // Callers publish their own failure result; keep the owner alive even
            // if an unexpected exception crosses a task boundary.
            try { task(); } catch (...) {}
            { std::lock_guard lock(mutex_); active_ = false; }
            idle_.notify_all();
        }
    }
    std::mutex mutex_;
    std::condition_variable ready_, idle_;
    std::deque<std::function<void()>> tasks_;
    bool stopping_ = false, active_ = false;
    std::thread thread_;
};
