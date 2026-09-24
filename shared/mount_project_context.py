"""Mount one project into an existing Linux container without restarting it.

Executed by the trusted host service, never by a harness. open_tree clones only
this directory; move_mount installs it after entering the target mount namespace.
No shell interpolation, host-root bind, or credentials enter the container.
"""
import ctypes
import errno
import os
import sys

source, leader, project = sys.argv[1:]
if not leader.isdecimal() or not project or any(c not in 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_' for c in project):
    raise SystemExit('Invalid context mount identity')
libc = ctypes.CDLL(None, use_errno=True)
libc.syscall.restype = ctypes.c_long

def checked(result):
    if result < 0:
        code = ctypes.get_errno()
        raise OSError(code, os.strerror(code))
    return result

# Pin source, namespace and container root before changing namespaces.
source_fd = os.open(source, os.O_PATH | os.O_DIRECTORY | os.O_NOFOLLOW)
source_stat = os.fstat(source_fd)
namespace_fd = os.open('/proc/' + leader + '/ns/mnt', os.O_RDONLY)
root_fd = os.open('/proc/' + leader + '/root', os.O_RDONLY | os.O_DIRECTORY)
# open_tree(AT_EMPTY_PATH | OPEN_TREE_CLONE | OPEN_TREE_CLOEXEC)
mount_fd = checked(libc.syscall(428, source_fd, ctypes.c_char_p(b''), 0x1000 | 1 | os.O_CLOEXEC))
checked(libc.setns(namespace_fd, 0x00020000))  # CLONE_NEWNS
os.fchdir(root_fd)
os.chroot('.')
os.chdir('/')
parent = os.open('/', os.O_RDONLY | os.O_DIRECTORY)
for name in ('run', 'sandboxed-context', project):
    try:
        os.mkdir(name, 0o755, dir_fd=parent)
    except FileExistsError:
        pass
    child = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)
    os.close(parent)
    parent = child
current = os.fstat(parent)
if (current.st_dev, current.st_ino) != (source_stat.st_dev, source_stat.st_ino):
    # MOVE_MOUNT_F_EMPTY_PATH | MOVE_MOUNT_T_EMPTY_PATH
    checked(libc.syscall(429, mount_fd, ctypes.c_char_p(b''), parent, ctypes.c_char_p(b''), 0x44))
