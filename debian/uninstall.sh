#!/bin/sh

cat <<'EOF'
ASense is managed by APT on this system.

Remove the application while retaining local configuration:
  sudo apt remove asense

Remove the application and ASense-owned configuration/state:
  sudo apt purge asense
EOF

exit 0
