# fc-userscan

Scans and registers GC roots for manually compiled programs on NixOS.

## Problem description

One can install fancy libs using `nix-env` and compile programs against them.
But, after a system update, the Nix garbage collector comes and pulls dynamic
link and other runtime dependencies out from under our manually compiled files.

This problem exists for some extent on all distros, but in NixOS it's amplified
by the fact that even smallest changes somewhere down the dependency chain will
change checksums from which Nix store paths are constructed.

## Solution

This tool allows to scan arbitrary directories and to register all Nix
dependencies found as GC roots so that they won't be taken away by the garbage
collector. Since plain string search is used, it works for both dynamic linkage
and other references, like config file paths.

## Example

Consider a Python virtualenv:

```ShellSession
$ nix-env --install python3
$ pyvenv myvenv
```

Now let's see if there are Nix store references present (hint: there are):

```ShellSession
$ fc-userscan -l myvenv
myvenv/bin/python3.5:
/nix/store/a5zbx856hyfgz2isz0j60i8w44i6av09-python3-3.5.2

myvenv/pyvenv.cfg:
/nix/store/a5zbx856hyfgz2isz0j60i8w44i6av09-python3-3.5.2
```

`fc-userscan` scans and registers Nix store references found either as symlinks
(like python3.5) or in files (pyvenv.cfg). The `-l` flag causes found references
to be dumped to stdout. At the same time, found references are registered with
the Nix garbage collector:

```ShellSession
$ ls -lR /nix/var/nix/gcroots/profiles/per-user/ckauhaus/home/ckauhaus/myvenv
/nix/var/nix/gcroots/profiles/per-user/ckauhaus/home/ckauhaus/myvenv:
lrwxrwxrwx 1 ckauhaus users 57 Aug 11 13:29 a5zbx856hyfgz2isz0j60i8w44i6av09 -> /nix/store/a5zbx856hyfgz2isz0j60i8w44i6av09-python3-3.5.2
drwxr-xr-x 2 ckauhaus users 46 Aug 11 13:29 bin

/nix/var/nix/gcroots/profiles/per-user/ckauhaus/home/ckauhaus/myvenv/bin:
lrwxrwxrwx 1 ckauhaus users 57 Aug 11 13:29 a5zbx856hyfgz2isz0j60i8w44i6av09 -> /nix/store/a5zbx856hyfgz2isz0j60i8w44i6av09-python3-3.5.2
```

All GC root registrations for a given dir $DIR go into
`/nix/var/nix/gcroots/profiles/per-user/$USER/$DIR` so this can easily be
inspected by the administrator. Should a reference vanish at the original
location, the registration will be cleaned up by the next run.

## Contact

The primary author of `fc-userscan` is [Christian
Kauhaus](mailto:kc@flyingcircus.io) or @ckauhaus on various online services.

## License

The software is licensed under a 3-clause BSD license.
