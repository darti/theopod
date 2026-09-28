# Theo Pod

Squelette Rust `no_std` pour l’ESP32-S3-Touch-ePaper-1.54.

## Préparation

```sh
just install
```

La configuration de Cargo dans `.cargo/config.toml` fournit automatiquement
les chemins nécessaires à la toolchain ESP. Aucun `export-esp.sh` n'est requis.

## Compiler et flasher

```sh
just check
just build
just flash                         # détection automatique du port
just flash /dev/cu.usbmodemXXXX   # port explicite
just monitor /dev/cu.usbmodemXXXX
just run /dev/cu.usbmodemXXXX     # flash puis moniteur série
```

Le firmware écrit actuellement `Hello world !` sur la sortie série. Le brochage exact de la carte sera ajouté avant les pilotes e-paper, tactile et audio.
