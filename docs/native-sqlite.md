# Shared SQLite for Python compatibility

The standalone Forge CLI uses bundled SQLite. The `conductor_native` Python
extension links dynamically to the same SQLite library as Python's `_sqlite3`
module. This lets the native A2A store and the public Python `connect()` API
observe each other's committed changes.

Loading two SQLite implementations into one process can invalidate their lock
bookkeeping. See SQLite's [multiple-library explanation](https://www.sqlite.org/howtocorrupt.html#multiple_copies_of_sqlite_linked_into_the_same_application).
The A2A constructor compares the actual `sqlite3_libversion` function addresses
before creating or opening a store. Different library identities are an error;
matching version strings alone are insufficient.

For a Linux Python linked to the system SQLite, install the SQLite development
package and `pkg-config` before building the extension. On Debian/Ubuntu:

```sh
sudo apt-get install libsqlite3-dev pkg-config
uv sync --extra test
```

If Python uses a different shared SQLite library, build against that library
using `SQLITE3_LIB_DIR`. A Python with a private static SQLite implementation is
not compatible with the in-process store binding; use a Python linked to a
shared SQLite runtime. The standalone Forge CLI remains self-contained.

Do not enable `bundled-sqlite` when building the Python extension. That feature
is selected by Forge's Cargo dependency for its separate native process. Wheel
repair is disabled for the extension so packaging cannot silently vendor a
second SQLite library. Such wheels retain their native platform requirements.
Maturin documents this setting in its [configuration reference](https://www.maturin.rs/config).

The migration was checked with native writes followed by Python reads,
including setting and clearing a retention hold. The old mixed-library build
reproduced a stale read immediately after a committed update; the shared-library
build passes the same regression.
