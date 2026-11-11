#!/usr/bin/env python3
# Copyright 2026 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Create empty files listed on the command line (Secular ffx_action stub)."""

import os
import sys


def main(argv: list[str]) -> int:
    for path in argv:
        directory = os.path.dirname(path)
        if directory:
            os.makedirs(directory, exist_ok=True)
        open(path, "ab").close()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
