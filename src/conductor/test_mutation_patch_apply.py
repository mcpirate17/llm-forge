"""What the anchored applier must and must not do.

The whole point of the module is that it is *more* tolerant than ``git apply``
about where a hunk sits and *no* more tolerant about which hunk it is. Every
test here is one half of that: either drift it must survive, or ambiguity it
must refuse. A test that only proved "the patch applied" would pass just as
well against a version that guessed.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from conductor.mutation_patch_apply import (
    PatchApplyError,
    apply_patch_text,
    check_patch_text,
    parse_unified_diff,
)

_PATCH = """diff --git a/m.py b/m.py
--- a/m.py
+++ b/m.py
@@ -2,5 +2,5 @@
 def f(x):
     y = x + 1
-    return y
+    return -y
 
 
"""


def _write(root: Path, body: str) -> Path:
    target = root / "m.py"
    target.write_text(body)
    return target


_ORIGINAL = "import os\ndef f(x):\n    y = x + 1\n    return y\n\n\n"


def test_a_hunk_applies_where_its_content_is_not_where_its_numbers_say(
    tmp_path: Path,
) -> None:
    """Twenty inserted lines above the hunk move every line number in it."""

    target = _write(tmp_path, "# pad\n" * 20 + _ORIGINAL)
    apply_patch_text(_PATCH, tmp_path)
    assert "return -y" in target.read_text()
    assert "return y\n" not in target.read_text()


def test_an_edit_to_a_neighbouring_context_line_does_not_retire_the_hunk(
    tmp_path: Path,
) -> None:
    """The construct the mutant edits is untouched; only its neighbour moved.

    This is the exact failure that retired fifteen patches in one refactor:
    the leading context line is rewritten, so git refuses, though `return y`
    -- the line the mutant actually perturbs -- is still there verbatim.
    """

    target = _write(tmp_path, _ORIGINAL.replace("y = x + 1", "y = x + 1  # note"))
    apply_patch_text(_PATCH, tmp_path)
    # Assert the WHOLE file. Checking only that the mutated line changed would
    # pass just as well against an applier that ate the untouched lines around
    # it -- which is precisely how a bad span calculation fails.
    assert target.read_text() == (
        "import os\ndef f(x):\n    y = x + 1  # note\n    return -y\n\n\n"
    )


def test_appending_past_an_eof_anchored_hunk_does_not_retire_it(
    tmp_path: Path,
) -> None:
    """A hunk whose trailing context is EOF applies once code follows it."""

    target = _write(tmp_path, _ORIGINAL + "def appended():\n    return 2\n")
    apply_patch_text(_PATCH, tmp_path)
    assert target.read_text() == (
        "import os\ndef f(x):\n    y = x + 1\n    return -y\n\n\n"
        "def appended():\n    return 2\n"
    )


def test_an_ambiguous_anchor_is_refused_rather_than_guessed(tmp_path: Path) -> None:
    """Two identical candidate sites EQUIDISTANT from the declared line.

    `git apply` never meets this case because a line number decides it. The
    applier searches, so it can -- and a coin flip here would apply a mutant to
    the wrong function while still reporting success. The two blocks start at
    lines 3 and 13 and the header declares 8, so the tiebreaker has nothing to
    prefer and must refuse rather than take the first.
    """

    block = "def f(x):\n    y = x + 1\n    return y\n\n\n"
    _write(tmp_path, "# a\n# b\n" + block + "# pad\n" * 5 + block)
    patch = _PATCH.replace("@@ -2,5 +2,5 @@", "@@ -8,5 +8,5 @@")
    with pytest.raises(PatchApplyError, match="refusing to guess"):
        apply_patch_text(patch, tmp_path)


def test_a_repeated_construct_is_resolved_by_the_declared_line_not_by_order(
    tmp_path: Path,
) -> None:
    """When the sites are NOT equidistant, `@@` breaks the tie as git would."""

    block = "def f(x):\n    y = x + 1\n    return y\n\n\n"
    target = _write(tmp_path, block + "# pad\n" * 40 + block)
    patch = _PATCH.replace("@@ -2,5 +2,5 @@", "@@ -46,5 +46,5 @@")
    apply_patch_text(patch, tmp_path)
    body = target.read_text()
    # The header points at the SECOND site, so that is the one that must change.
    # Pointing it at the first would pass against an applier that simply took
    # the earliest match and never read the header at all.
    assert body.index("return -y") > body.index("# pad")


def test_a_hunk_whose_construct_is_gone_is_refused(tmp_path: Path) -> None:
    """Relaxation is on absence of context, never on absence of the edit."""

    _write(tmp_path, _ORIGINAL.replace("    return y", "    return abs(y)"))
    with pytest.raises(PatchApplyError, match="does not match anywhere"):
        apply_patch_text(_PATCH, tmp_path)


def test_relaxing_context_keeps_the_edited_line_as_the_anchor(
    tmp_path: Path,
) -> None:
    """Every context line differs; only the mutated line survives.

    Relaxation drops context from both ends, so this hunk ends up anchored on
    `return y` alone -- which is exactly the line it edits, and is unique here.
    It must land there without disturbing the differing neighbours.
    """

    _write(tmp_path, "def g(z):\n    w = z * 3\n    return y\n\n\n")
    apply_patch_text(_PATCH, tmp_path)
    # `return y` is unique, so this one legitimately applies; assert it landed
    # on that line and did not disturb the differing context.
    body = (tmp_path / "m.py").read_text()
    assert "return -y" in body
    assert "w = z * 3" in body


def test_a_file_with_no_trailing_newline_keeps_that_property(
    tmp_path: Path,
) -> None:
    patch = """diff --git a/m.py b/m.py
--- a/m.py
+++ b/m.py
@@ -1,1 +1,1 @@
-value = 1
\\ No newline at end of file
+value = 2
\\ No newline at end of file
"""
    target = tmp_path / "m.py"
    target.write_text("value = 1")
    apply_patch_text(patch, tmp_path)
    assert target.read_text() == "value = 2"


def test_a_pure_insertion_hunk_places_the_new_lines(tmp_path: Path) -> None:
    """A hunk with no `-` lines has no edited line to anchor on."""

    patch = """diff --git a/m.py b/m.py
--- a/m.py
+++ b/m.py
@@ -1,3 +1,4 @@
 import os
 def f(x):
+    assert x
     y = x + 1
"""
    target = _write(tmp_path, _ORIGINAL)
    apply_patch_text(patch, tmp_path)
    # Whole file: an insertion must add lines and remove none, so a span that
    # is one line too wide has to show up as a missing `return y`.
    assert target.read_text() == (
        "import os\ndef f(x):\n    assert x\n    y = x + 1\n    return y\n\n\n"
    )


def test_a_pure_insertions_leading_anchor_is_never_relaxed_away(
    tmp_path: Path,
) -> None:
    """An insertion has no edited line, so its context is the only anchor.

    Relaxing that away would place the new lines by trailing context alone --
    a guess about where they belong. With the leading anchor gone the hunk is
    refused instead.
    """

    patch = """diff --git a/m.py b/m.py
--- a/m.py
+++ b/m.py
@@ -1,4 +1,5 @@
 import os
+import sys
 def f(x):
     y = x + 1
     return y
"""
    _write(tmp_path, _ORIGINAL.replace("import os", "import io"))
    with pytest.raises(PatchApplyError, match="does not match anywhere"):
        apply_patch_text(patch, tmp_path)


def test_a_patch_that_creates_or_deletes_a_file_is_refused(tmp_path: Path) -> None:
    """A mutant edits code in place; anything else is not a mutant.

    The patch carries a real hunk, so it would parse fine without the directive
    check. Without that, this test would pass on "patch has no hunks" and prove
    nothing about creations at all.
    """

    for marker in ("new file mode 100644", "deleted file mode 100644", "rename from x"):
        patch = (
            f"diff --git a/m.py b/m.py\n{marker}\n--- a/m.py\n+++ b/m.py\n"
            "@@ -1,1 +1,1 @@\n-import os\n+import sys\n"
        )
        with pytest.raises(PatchApplyError, match="unsupported patch directive"):
            parse_unified_diff(patch)


def test_a_dev_null_source_or_target_is_refused(tmp_path: Path) -> None:
    """The other spelling of a creation or deletion."""

    created = (
        "diff --git a/m.py b/m.py\n--- /dev/null\n+++ b/m.py\n"
        "@@ -0,0 +1,1 @@\n+import sys\n"
    )
    with pytest.raises(PatchApplyError, match="creates a file"):
        parse_unified_diff(created)
    deleted = (
        "diff --git a/m.py b/m.py\n--- a/m.py\n+++ /dev/null\n"
        "@@ -1,1 +0,0 @@\n-import os\n"
    )
    with pytest.raises(PatchApplyError, match="deletes a file"):
        parse_unified_diff(deleted)


def test_a_patch_targeting_a_missing_file_is_refused(tmp_path: Path) -> None:
    with pytest.raises(PatchApplyError, match="missing file"):
        check_patch_text(_PATCH, tmp_path)


def test_check_reports_the_same_answer_as_apply_without_writing(
    tmp_path: Path,
) -> None:
    target = _write(tmp_path, _ORIGINAL)
    before = target.read_text()
    check_patch_text(_PATCH, tmp_path)
    assert target.read_text() == before
    apply_patch_text(_PATCH, tmp_path)
    assert target.read_text() != before


def test_apply_reports_every_path_it_touched(tmp_path: Path) -> None:
    _write(tmp_path, _ORIGINAL)
    assert apply_patch_text(_PATCH, tmp_path) == ("m.py",)
