# Tweets to Discord

This is a bot I made to stream a Twitter feed and post updates to Discord. It
was initially named `runar-tweets-to-discord` since the idea behind this was to
ensure all posts posted by [Runar Devik
Søgaard](https://twitter.com/RunarSogaard) would end up in our discord server in
real time. However, after finishing the implementation I realised this could be
used for anyone.

Currently, this only supports a single user feed to follow. It will post all
mesages (mentions and replies).

## Usage

Edit the configuration file and run it.

```sh
% cargo run
```
