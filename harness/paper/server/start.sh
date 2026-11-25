#!/bin/sh
# The harness container's start (ADR-0049): a new RCON password for this
# container, which `rcon` reads back, then Paper in the foreground.
set -eu
cd /paper
password=$(od -An -tx1 -N16 /dev/urandom | tr -d ' \n')
grep -v '^rcon.password=' server.properties > server.properties.new || true
echo "rcon.password=$password" >> server.properties.new
mv server.properties.new server.properties
exec java -Xms1G -Xmx1G -jar paper.jar --nogui
