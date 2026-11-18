// SPDX-License-Identifier: MIT
// Tiny fcitx5 C++ addon exposing `/y700osk` (`fyi.y700.OskCommit1`) on `org.fcitx.Fcitx5`
// so `tb520fu-osk` can commit CJK punctuation, emoji, symbols, and clipboard text directly
// into the focused input context (`ic->commitString(text)`) without touching the clipboard.
// (If not installed, `tb520fu-osk` automatically falls back to `wl-copy` + `Ctrl+V`.)

#include <fcitx-utils/dbus/bus.h>
#include <fcitx-utils/dbus/objectvtable.h>
#include <fcitx-utils/key.h>
#include <fcitx/addonfactory.h>
#include <fcitx/addoninstance.h>
#include <fcitx/addonmanager.h>
#include <fcitx/candidatelist.h>
#include <fcitx/event.h>
#include <fcitx/inputcontext.h>
#include <fcitx/inputcontextmanager.h>
#include <fcitx/inputpanel.h>
#include <fcitx/instance.h>
#include "dbus_public.h"

namespace fcitx {

class OskCommitService : public dbus::ObjectVTable<OskCommitService> {
public:
    explicit OskCommitService(Instance *instance) : instance_(instance) {}

    bool commitString(const std::string &text) {
        auto *ic = focused();
        if (!ic || text.empty()) {
            return false;
        }
        ic->commitString(text);
        return true;
    }

    bool forwardKey(const std::string &name, uint32_t xkbCode, bool isRelease) {
        auto *ic = focused();
        if (!ic) {
            return false;
        }
        Key parsed(name);
        Key key(parsed.sym(), parsed.states(), static_cast<int>(xkbCode));
        if (!key.isValid()) {
            throw dbus::MethodCallError("org.freedesktop.DBus.Error.InvalidArgs", "bad key");
        }
        ic->forwardKey(key, isRelease);
        return true;
    }

    bool hasFocus() { return focused() != nullptr; }

private:
    InputContext *focused() {
        auto *ic = instance_->inputContextManager().lastFocusedInputContext();
        return (ic && ic->hasFocus()) ? ic : nullptr;
    }

    FCITX_OBJECT_VTABLE_METHOD(commitString, "CommitString", "s", "b");
    FCITX_OBJECT_VTABLE_METHOD(forwardKey, "ForwardKey", "sub", "b");
    FCITX_OBJECT_VTABLE_METHOD(hasFocus, "HasFocus", "", "b");
    Instance *instance_;
};

class Y700OskAddon : public AddonInstance {
public:
    explicit Y700OskAddon(Instance *instance);

private:
    Instance *instance_;
    FCITX_ADDON_DEPENDENCY_LOADER(dbus, instance_->addonManager());
    OskCommitService service_;
};

Y700OskAddon::Y700OskAddon(Instance *instance) : instance_(instance), service_(instance) {
    dbus()->call<IDBusModule::bus>()->addObjectVTable("/y700osk", "fyi.y700.OskCommit1", service_);
}

class Y700OskFactory : public AddonFactory {
    AddonInstance *create(AddonManager *manager) override {
        return new Y700OskAddon(manager->instance());
    }
};

} // namespace fcitx

FCITX_ADDON_FACTORY_V2(y700osk, fcitx::Y700OskFactory);
