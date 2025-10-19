# livesplit-for-sober
Forked from [live-split-hotkeys](https://github.com/descawed/live-split-hotkeys/tree/master) and merged pulls for fixes from [alexankitty](https://github.com/descawed/live-split-hotkeys/pull/4)

This is NOT the official repository, and simply a fork. If you have anything relating to the live-split-hotkeys build, refer to the official repository stated earlier.

## Installation
> [!CAUTION]
> This guide has only been tested with Arch Linux. Do not attempt to run or convert these commands to your distro without sufficient knowledge.

- Install Rust and Cargo:
  - `sudo pacman -S rustup`
  
- Set to stable toolchain:
  - `rustup default stable`
 
- Clone the repository:
  - `git clone https://github.com/lightwork-speedrunning/livesplit-for-sober.git`

- Navigate into the directory:
  - `cd live-split-hotkeys`

- Build executable:
  - `cargo build --release`

- Building should supply you with an executable file. Simply run your LiveSplit, start a TCP server, then run the initialization command:
  - `sudo /path/to/file/live-split-hotkeys -s /path/to/file/LiveSplit/settings.cfg`
