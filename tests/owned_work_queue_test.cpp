#include "owned_work_queue.h"
#include "revert_guard.h"
#include <cassert>
#include <future>
#include <atomic>
#include <chrono>
#include <vector>
using namespace std::chrono_literals;
int main() {
    RevertGuard reverted;
    reverted.record("entity-summon");
    // A failed write/finished query cannot clear a pending mutation.
    reverted.forgetSettled();
    assert(reverted.contains("entity-summon"));
    reverted.acknowledge("entity-summon");
    assert(reverted.contains("entity-summon")); // older query can still contain it
    reverted.forgetSettled();
    assert(!reverted.contains("entity-summon"));
    RevertGuard recovered;
    recovered.record("entity-summon"); // recovery status entry restores protection
    recovered.forgetSettled();
    assert(recovered.contains("entity-summon"));

    // Join must wait for the actual owner, drain accepted tasks, and reject new work.
    OwnedWorkQueue queue;
    std::promise<void> entered, release;
    auto gate = release.get_future().share();
    std::vector<int> completed;
    queue.submit([&] { entered.set_value(); gate.wait(); completed.push_back(0); });
    entered.get_future().wait();
    for (int i = 1; i <= 256; ++i) queue.submit([&, i] { completed.push_back(i); });
    bool full = false;
    try { queue.submit([] {}); } catch (const std::runtime_error&) { full = true; }
    assert(full);
    auto stopped = std::async(std::launch::async, [&] { queue.stop(); });
    assert(stopped.wait_for(20ms) == std::future_status::timeout);
    release.set_value(); stopped.get();
    assert(completed.size() == 257);
    for (int i = 0; i <= 256; ++i) assert(completed[i] == i);
    bool closed = false;
    try { queue.submit([] {}); } catch (const std::runtime_error&) { closed = true; }
    assert(closed);
    // An unexpected task exception must not strand later accepted work or joins.
    queue.start();
    queue.submit([] { throw std::runtime_error("injected failure"); });
    queue.submit([&] { completed.push_back(257); });
    queue.waitIdle(); assert(completed.back() == 257);
    queue.stop();
}
