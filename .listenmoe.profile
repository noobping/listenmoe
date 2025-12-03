# Basic includes
include /etc/firejail/disable-common.inc
include /etc/firejail/whitelist-var-common.inc

# DO NOT block the network
# (no "net none" line here)
private-etc resolv.conf
