package afkfleet.harness;

import java.lang.reflect.Field;
import java.math.BigDecimal;
import java.util.Arrays;
import java.util.HashMap;
import java.util.Map;
import java.util.Optional;
import java.util.Queue;
import java.util.UUID;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.ConcurrentLinkedQueue;

import io.netty.channel.Channel;
import io.netty.channel.ChannelHandlerContext;
import io.netty.channel.ChannelInboundHandlerAdapter;
import io.papermc.paper.event.packet.UncheckedSignChangeEvent;
import net.kyori.adventure.text.Component;
import net.kyori.adventure.text.format.NamedTextColor;
import net.kyori.adventure.text.serializer.plain.PlainTextComponentSerializer;
import org.bukkit.Bukkit;
import org.bukkit.command.Command;
import org.bukkit.command.CommandSender;
import org.bukkit.command.ConsoleCommandSender;
import org.bukkit.command.RemoteConsoleCommandSender;
import org.bukkit.entity.HumanEntity;
import org.bukkit.entity.Player;
import org.bukkit.event.EventHandler;
import org.bukkit.event.EventPriority;
import org.bukkit.event.Listener;
import org.bukkit.event.inventory.InventoryClickEvent;
import org.bukkit.event.inventory.InventoryCloseEvent;
import org.bukkit.event.inventory.InventoryDragEvent;
import org.bukkit.event.inventory.InventoryOpenEvent;
import org.bukkit.event.inventory.PrepareAnvilEvent;
import org.bukkit.event.player.PlayerDropItemEvent;
import org.bukkit.event.player.PlayerItemConsumeEvent;
import org.bukkit.event.player.PlayerJoinEvent;
import org.bukkit.event.player.PlayerKickEvent;
import org.bukkit.event.player.PlayerQuitEvent;
import org.bukkit.inventory.InventoryView;
import org.bukkit.inventory.ItemStack;
import org.bukkit.plugin.java.JavaPlugin;

/**
 * afkfleet's test plugin (Plan.md P16.1, ADR-0049): an auction house and a sell menu as a server's
 * plugins build them, a small economy, and a log line for every click with the server's tick and
 * time, so the tests and a person with their own client can see what a click did and when.
 *
 * <p>Every line it logs starts with {@code harness-}: {@code harness-click}, {@code harness-drag},
 * {@code harness-open}, {@code harness-close}, {@code harness-kick}, {@code harness-bought},
 * {@code harness-refused}, {@code harness-listed}, {@code harness-expired}, {@code
 * harness-collected}, {@code harness-sold}, {@code harness-sign-open}, {@code harness-sign},
 * and for the live tests (Plan.md P16.17, P16.18) {@code harness-consume}, {@code harness-gone},
 * {@code harness-drop}, {@code harness-snipe}, {@code harness-intrude}, {@code harness-stalled} and {@code
 * harness-kickon}, {@code harness-ignored} for a click packet no handler got. A click's line says
 * the window and state the server had, the state its packet carried and the listing it met.
 */
public final class HarnessPlugin extends JavaPlugin implements Listener {
    private final Market market = new Market();
    private Menus menus;

    @Override
    public void onEnable() {
        menus = new Menus(this, market);
        getServer().getPluginManager().registerEvents(this, this);
        Bukkit.getScheduler().runTaskTimer(this, () -> {
            int expired = market.expireDue(Bukkit.getCurrentTick());
            if (expired > 0) {
                getLogger().info("harness-expired count=" + expired);
                menus.marketChanged();
            }
        }, 20, 20);
    }

    // --- The log -------------------------------------------------------------------------------

    private static String plain(Component text) {
        return PlainTextComponentSerializer.plainText().serialize(text);
    }

    private static String title(InventoryView view) {
        return '"' + plain(view.title()) + '"';
    }

    void log(String what) {
        getLogger().info(what + " tick=" + Bukkit.getCurrentTick() + " nanos=" + System.nanoTime());
    }

    /** What each player's click met before any handler ran: the window, its state and the listing. */
    private final Map<UUID, String> metAtClick = new HashMap<>();

    /**
     * Each player's click packets as the server read them off the network, before it handled any:
     * window, the state the packet carried, slot. Written on the network's thread, read on the main
     * one.
     */
    private final Map<UUID, Queue<int[]>> clickPackets = new ConcurrentHashMap<>();

    /**
     * Before every handler: the window and state the server has for the clicker's open screen,
     * the state the click's own packet carried ({@code sent=}), and the listing the clicked slot
     * holds, so the click's line says what the click met and what it was sent onto, not what a
     * handler made of it (Plan.md P16.18). A packet read before it whose window the server no longer
     * had open reached no handler: it's logged as {@code harness-ignored}.
     */
    @EventHandler(priority = EventPriority.LOWEST)
    public void meetClick(InventoryClickEvent event) {
        HumanEntity clicker = event.getWhoClicked();
        int[] now = windowAndStateOf(clicker);
        String sent = "?";
        Queue<int[]> packets = clickPackets.get(clicker.getUniqueId());
        if (packets != null && now != null) {
            for (int[] packet = packets.poll(); packet != null; packet = packets.poll()) {
                if (packet[0] == now[0] && packet[2] == event.getRawSlot()) {
                    sent = Integer.toString(packet[1]);
                    break;
                }
                log("harness-ignored player=" + clicker.getName() + " window=" + packet[0] + " sent="
                        + packet[1] + " raw=" + packet[2]);
            }
        }
        metAtClick.put(clicker.getUniqueId(),
                windowAndState(clicker) + " sent=" + sent + " listing=" + menus.listingAt(event));
    }

    /**
     * Reads every click packet of a player who joins, off the network before the server handles it
     * (Plan.md P16.18): the server's handler drops a click for a window it no longer has open and
     * applies one with an older state anyway, and no event says either. A Netty handler before
     * Paper's {@code packet_handler}, found through the server's own objects (Paper runs Mojang's
     * names); if they can't be read, a warning says so, which fails the tests that count on it.
     */
    @EventHandler(priority = EventPriority.MONITOR)
    public void watchClicks(PlayerJoinEvent event) {
        Player player = event.getPlayer();
        Queue<int[]> packets = new ConcurrentLinkedQueue<>();
        clickPackets.put(player.getUniqueId(), packets);
        try {
            Object handle = player.getClass().getMethod("getHandle").invoke(player);
            Object listener = field(handle, "connection");
            Object connection = field(listener, "connection");
            Channel channel = (Channel) field(connection, "channel");
            channel.pipeline().addBefore("packet_handler", "afkfleet-harness-clicks", new ClickReader(packets));
        } catch (ReflectiveOperationException | RuntimeException unwatched) {
            getLogger().warning("harness-unwatched player=" + player.getName() + " why=" + unwatched);
        }
    }

    @EventHandler(priority = EventPriority.MONITOR)
    public void forgetClicks(PlayerQuitEvent event) {
        clickPackets.remove(event.getPlayer().getUniqueId());
    }

    /** Notes each container click packet's window, state and slot as the network reads it. */
    private static final class ClickReader extends ChannelInboundHandlerAdapter {
        private final Queue<int[]> packets;

        ClickReader(Queue<int[]> packets) {
            this.packets = packets;
        }

        @Override
        public void channelRead(ChannelHandlerContext context, Object message) throws Exception {
            if (message.getClass().getSimpleName().equals("ServerboundContainerClickPacket")) {
                packets.add(new int[] {accessor(message, "containerId"), accessor(message, "stateId"),
                    accessor(message, "slotNum")});
            }
            super.channelRead(context, message);
        }
    }

    /** {@code owner}'s field {@code name}, declared in its class or any above it, whatever its access. */
    private static Object field(Object owner, String name) throws ReflectiveOperationException {
        for (Class<?> type = owner.getClass(); type != null; type = type.getSuperclass()) {
            try {
                Field field = type.getDeclaredField(name);
                field.setAccessible(true);
                return field.get(owner);
            } catch (NoSuchFieldException above) {
                // Declared further up.
            }
        }
        throw new NoSuchFieldException(name);
    }

    /** A record's accessor {@code name} as a whole number, or -1. */
    private static int accessor(Object packet, String name) {
        try {
            return ((Number) packet.getClass().getMethod(name).invoke(packet)).intValue();
        } catch (ReflectiveOperationException | RuntimeException unread) {
            return -1;
        }
    }

    /** The window and state the server has for {@code player}'s open screen, or null. */
    static int[] windowAndStateOf(HumanEntity player) {
        try {
            Object handle = player.getClass().getMethod("getHandle").invoke(player);
            Object menu = handle.getClass().getField("containerMenu").get(handle);
            int window = menu.getClass().getField("containerId").getInt(menu);
            int state = ((Number) menu.getClass().getMethod("getStateId").invoke(menu)).intValue();
            return new int[] {window, state};
        } catch (ReflectiveOperationException | RuntimeException unread) {
            return null;
        }
    }

    /** Every click, after every handler, with whether it was cancelled and what it met. */
    @EventHandler(priority = EventPriority.MONITOR)
    public void logClick(InventoryClickEvent event) {
        String met = metAtClick.remove(event.getWhoClicked().getUniqueId());
        log("harness-click player=" + event.getWhoClicked().getName() + " screen=" + title(event.getView())
                + " raw=" + event.getRawSlot() + " slot=" + event.getSlot() + " click=" + event.getClick()
                + " action=" + event.getAction() + " hotbar=" + event.getHotbarButton()
                + " cancelled=" + event.isCancelled() + " " + (met == null ? "window=? state=? listing=-" : met));
    }

    /**
     * The window and state IDs the server has for {@code player}'s open screen, as {@code window=…
     * state=…}: read from the server's own player object (Paper runs Mojang's names), since no
     * event says them. A click carries the state its sender last had; the server applies it
     * anyway, so this is what the click met.
     */
    static String windowAndState(HumanEntity player) {
        int[] now = windowAndStateOf(player);
        return now == null ? "window=? state=?" : "window=" + now[0] + " state=" + now[1];
    }

    /** Every item a player drops, from a screen or the hand (Plan.md P16.17). */
    @EventHandler(priority = EventPriority.MONITOR)
    public void logDrop(PlayerDropItemEvent event) {
        ItemStack dropped = event.getItemDrop().getItemStack();
        log("harness-drop player=" + event.getPlayer().getName() + " item=" + Market.sellKey(dropped)
                + " count=" + dropped.getAmount() + " cancelled=" + event.isCancelled());
    }

    /** Every item a player finishes using: a potion drunk, food eaten (Plan.md P16.17). */
    @EventHandler(priority = EventPriority.MONITOR)
    public void logConsume(PlayerItemConsumeEvent event) {
        log("harness-consume player=" + event.getPlayer().getName() + " item=" + Market.sellKey(event.getItem())
                + " cancelled=" + event.isCancelled());
    }

    @EventHandler(priority = EventPriority.MONITOR)
    public void logDrag(InventoryDragEvent event) {
        log("harness-drag player=" + event.getWhoClicked().getName() + " screen=" + title(event.getView())
                + " slots=" + event.getRawSlots() + " cancelled=" + event.isCancelled());
    }

    @EventHandler(priority = EventPriority.MONITOR)
    public void logOpen(InventoryOpenEvent event) {
        log("harness-open player=" + event.getPlayer().getName() + " screen=" + title(event.getView())
                + " cancelled=" + event.isCancelled() + " listing=" + menus.listingOf(event.getView()));
        if (!event.isCancelled() && event.getPlayer() instanceof Player player) {
            menus.opened(player, plain(event.getView().title()), event.getView());
        }
    }

    @EventHandler(priority = EventPriority.MONITOR)
    public void logClose(InventoryCloseEvent event) {
        log("harness-close player=" + event.getPlayer().getName() + " screen=" + title(event.getView())
                + " reason=" + event.getReason());
    }

    @EventHandler(priority = EventPriority.MONITOR)
    public void logKick(PlayerKickEvent event) {
        log("harness-kick player=" + event.getPlayer().getName() + " cause=" + event.getCause()
                + " reason=\"" + plain(event.reason()) + '"');
    }

    // --- The menus -----------------------------------------------------------------------------

    @EventHandler
    public void onClick(InventoryClickEvent event) {
        menus.onClick(event);
    }

    /**
     * After the click's own line: a player armed with {@code harness kickon} is kicked inside the
     * click, so whatever they send behind it is never answered (Plan.md P16.17).
     */
    @EventHandler(priority = EventPriority.MONITOR)
    public void kickAtClick(InventoryClickEvent event) {
        if (event.getWhoClicked() instanceof Player player && menus.kicksAt(player)) {
            log("harness-kickon player=" + player.getName());
            player.kick(Component.text("The harness kicked you mid-click."));
        }
    }

    @EventHandler
    public void onDrag(InventoryDragEvent event) {
        menus.onDrag(event);
    }

    @EventHandler
    public void onClose(InventoryCloseEvent event) {
        menus.onClose(event);
    }

    @EventHandler
    public void onPrepareAnvil(PrepareAnvilEvent event) {
        menus.onPrepareAnvil(event);
    }

    @EventHandler
    public void onSign(UncheckedSignChangeEvent event) {
        menus.onSign(event);
    }

    /** Buys listing `id` for `buyer`, the confirm button's and `harness buy`'s one way. */
    Optional<Market.Refusal> purchase(Player buyer, int id) {
        Optional<Market.Listing> listing = market.listing(id);
        boolean fits = buyer.getInventory().firstEmpty() != -1;
        Optional<Market.Refusal> refused = market.buy(buyer.getName(), id, fits);
        if (refused.isPresent()) {
            log("harness-refused id=" + id + " buyer=" + buyer.getName() + " reason=" + refused.get());
            buyer.sendMessage(Component.text(refused.get().text, NamedTextColor.RED));
            return refused;
        }
        Market.Listing bought = listing.orElseThrow();
        Menus.giveOrDrop(buyer, bought.item().clone());
        log("harness-bought id=" + id + " buyer=" + buyer.getName() + " seller=" + bought.seller()
                + " price=" + bought.price().toPlainString());
        menus.marketChanged();
        buyer.sendMessage(Component.text("You bought it for " + Market.money(bought.price()) + ".",
                NamedTextColor.GREEN));
        Player seller = Bukkit.getPlayerExact(bought.seller());
        if (seller != null) {
            seller.sendMessage(Component.text("Your listing sold for " + Market.money(bought.price()) + ".",
                    NamedTextColor.GREEN));
        }
        return Optional.empty();
    }

    // --- Commands ------------------------------------------------------------------------------

    @Override
    public boolean onCommand(CommandSender sender, Command command, String label, String[] args) {
        if (command.getName().equals("harness")) {
            return harness(sender, args);
        }
        if (!(sender instanceof Player player)) {
            sender.sendMessage("Players only.");
            return true;
        }
        switch (command.getName()) {
            case "ah" -> ah(player, args);
            case "sell" -> menus.openSell(player);
            case "balance" -> player.sendMessage(Component.text(
                    "Balance: " + Market.money(market.balance(player.getName())), NamedTextColor.GOLD));
            case "pay" -> pay(player, args);
            default -> {
                return false;
            }
        }
        return true;
    }

    private void ah(Player player, String[] args) {
        if (args.length == 0) {
            menus.openAuction(player, 0);
        } else if (args[0].equalsIgnoreCase("sell")) {
            Optional<BigDecimal> price = args.length == 2 ? Market.parsePrice(args[1]) : Optional.empty();
            if (price.isEmpty()) {
                player.sendMessage(Component.text("Usage: /ah sell <price>, a price above 0.", NamedTextColor.RED));
                return;
            }
            menus.openConfirmListing(player, price.get());
        } else if (args[0].equalsIgnoreCase("expired")) {
            menus.openExpired(player);
        } else {
            try {
                menus.openAuction(player, Math.max(0, Integer.parseInt(args[0]) - 1));
            } catch (NumberFormatException notAPage) {
                // Anything else is a search, as on the user's server.
                menus.openAuction(player, 0, String.join(" ", args));
            }
        }
    }

    private void pay(Player player, String[] args) {
        Optional<BigDecimal> amount = args.length == 2 ? Market.parsePrice(args[1]) : Optional.empty();
        if (amount.isEmpty()) {
            player.sendMessage(Component.text("Usage: /pay <player> <amount>", NamedTextColor.RED));
            return;
        }
        if (!market.pay(player.getName(), args[0], amount.get())) {
            player.sendMessage(Component.text("You don't have enough money.", NamedTextColor.RED));
            return;
        }
        player.sendMessage(Component.text("You paid " + args[0] + " " + Market.money(amount.get()) + ".",
                NamedTextColor.GREEN));
    }

    /**
     * The tests' controls, from the console or RCON only: {@code money <player> <amount>}, {@code
     * balance <player>}, {@code list <seller> <price> <item>} (the item in the game's own syntax,
     * components included), {@code listings}, {@code expire <id>}, {@code expiry <seconds>}, {@code
     * buy <online player> <id>}, {@code price <key> <amount>}, {@code open <player> ah|sell|expired},
     * {@code sign <player>} (a sign editor nobody asked for) and {@code reset}; and for the live tests
     * (Plan.md P16.17, P16.18): {@code state <player>}, {@code count <item key> <max>}, {@code live on|off} (open auction houses
     * redrawn at every change of the market), {@code cap <price>} (the price anvil's highest),
     * {@code stall on|off} (Confirm Item's confirm does nothing), {@code snipe <buyer> house <item
     * key> <max>} and {@code snipe <buyer> confirm} (the other player buys first, the tick after
     * another player opens that screen), {@code intrude <player> <menu> <title...>} (a menu opened
     * unasked the tick after they open that title) and {@code kickon <player>} (kicked inside
     * their next click).
     */
    private boolean harness(CommandSender sender, String[] args) {
        if (!(sender instanceof ConsoleCommandSender) && !(sender instanceof RemoteConsoleCommandSender)) {
            sender.sendMessage("The console's only.");
            return true;
        }
        String verb = args.length == 0 ? "help" : args[0];
        try {
            switch (verb) {
                case "money" -> {
                    market.setBalance(args[1], new BigDecimal(args[2]));
                    sender.sendMessage("balance " + args[1] + " " + market.balance(args[1]).toPlainString());
                }
                case "balance" -> sender.sendMessage("balance " + args[1] + " " + market.balance(args[1]).toPlainString());
                case "count" -> {
                    // How many listings of an item (its key as the sell menu has it) cost at most
                    // a price: a short answer, where `listings` outgrows one RCON packet.
                    BigDecimal most = new BigDecimal(args[2]);
                    long count = market.listings().stream()
                            .filter(listing -> Market.sellKey(listing.item()).equals(args[1]))
                            .filter(listing -> listing.price().compareTo(most) <= 0)
                            .count();
                    sender.sendMessage("count " + count);
                }
                case "list" -> {
                    ItemStack item = Bukkit.getItemFactory().createItemStack(
                            String.join(" ", Arrays.copyOfRange(args, 3, args.length)));
                    BigDecimal price = Market.parsePrice(args[2])
                            .orElseThrow(() -> new IllegalArgumentException("not a price: " + args[2]));
                    Market.Listing listing = market.list(args[1], item, price, Bukkit.getCurrentTick());
                    log("harness-listed id=" + listing.id() + " seller=" + args[1] + " price="
                            + listing.price().toPlainString());
                    menus.marketChanged();
                    sender.sendMessage("listed " + listing.id());
                }
                case "fill" -> {
                    // `count` listings by Seller1 to Seller3: Turtle Master potions of every
                    // kind and form, a sword and a book, at 1,000 plus 10 a listing.
                    String[] items = {
                        "minecraft:potion[potion_contents={potion:\"minecraft:turtle_master\"}]",
                        "minecraft:potion[potion_contents={potion:\"minecraft:long_turtle_master\"}]",
                        "minecraft:splash_potion[potion_contents={potion:\"minecraft:strong_turtle_master\"}]",
                        "minecraft:lingering_potion[potion_contents={potion:\"minecraft:turtle_master\"}]",
                        "minecraft:tipped_arrow[potion_contents={potion:\"minecraft:turtle_master\"}]",
                        "minecraft:diamond_sword[enchantments={\"minecraft:sharpness\":5}]",
                        "minecraft:enchanted_book[stored_enchantments={\"minecraft:mending\":1}]",
                    };
                    int count = Integer.parseInt(args[1]);
                    for (int n = 0; n < count; n++) {
                        ItemStack item = Bukkit.getItemFactory().createItemStack(items[n % items.length]);
                        market.list("Seller" + (n % 3 + 1), item, BigDecimal.valueOf(1000 + 10L * n),
                                Bukkit.getCurrentTick());
                    }
                    menus.marketChanged();
                    sender.sendMessage("filled " + count);
                }
                case "listings" -> {
                    StringBuilder out = new StringBuilder("listings " + market.listings().size());
                    for (Market.Listing listing : market.listings()) {
                        out.append("\n").append(listing.id()).append(' ').append(listing.seller()).append(' ')
                                .append(listing.price().toPlainString()).append(' ')
                                .append(listing.item().getType().getKey());
                    }
                    sender.sendMessage(out.toString());
                }
                case "expire" -> {
                    boolean expired = market.expire(Integer.parseInt(args[1]));
                    menus.marketChanged();
                    sender.sendMessage(expired ? "expired" : "no such listing");
                }
                case "expiry" -> {
                    market.setExpiryTicks(20L * Long.parseLong(args[1]));
                    sender.sendMessage("expiry " + args[1] + "s");
                }
                case "buy" -> {
                    Player buyer = Bukkit.getPlayerExact(args[1]);
                    if (buyer == null) {
                        sender.sendMessage("not online");
                    } else {
                        Optional<Market.Refusal> refused = purchase(buyer, Integer.parseInt(args[2]));
                        sender.sendMessage(refused.map(refusal -> "refused " + refusal).orElse("bought"));
                    }
                }
                case "price" -> {
                    market.setPrice(args[1], new BigDecimal(args[2]));
                    sender.sendMessage("price " + args[1] + " " + args[2]);
                }
                case "open" -> {
                    Player player = Bukkit.getPlayerExact(args[1]);
                    if (player == null) {
                        sender.sendMessage("not online");
                    } else {
                        sender.sendMessage(menus.open(player, args[2]) ? "opened" : "no such menu");
                    }
                }
                case "sign" -> {
                    Player player = Bukkit.getPlayerExact(args[1]);
                    if (player == null) {
                        sender.sendMessage("not online");
                    } else {
                        menus.openSign(player);
                        sender.sendMessage("sign opened");
                    }
                }
                case "reset" -> {
                    market.reset();
                    menus.marketChanged();
                    sender.sendMessage("reset");
                }
                case "state" -> {
                    Player player = Bukkit.getPlayerExact(args[1]);
                    sender.sendMessage(player == null ? "not online" : menus.state(player));
                }
                case "live" -> {
                    menus.setLive(args[1].equals("on"));
                    sender.sendMessage("live " + args[1]);
                }
                case "cap" -> {
                    menus.setPriceCap(Long.parseLong(args[1]));
                    sender.sendMessage("cap " + args[1]);
                }
                case "stall" -> {
                    menus.setStalled(args[1].equals("on"));
                    sender.sendMessage("stall " + args[1]);
                }
                case "snipe" -> {
                    Menus.Snipe snipe = args[2].equals("house")
                            ? new Menus.Snipe(args[1], "house", args[3], new BigDecimal(args[4]))
                            : new Menus.Snipe(args[1], "confirm", "", BigDecimal.ZERO);
                    menus.arm(snipe);
                    sender.sendMessage("snipe armed");
                }
                case "intrude" -> {
                    String after = String.join(" ", Arrays.copyOfRange(args, 3, args.length));
                    menus.intrude(new Menus.Intrusion(args[1], after, args[2]));
                    sender.sendMessage("intrusion armed");
                }
                case "kickon" -> {
                    menus.kickAtClick(args[1]);
                    sender.sendMessage("kick armed");
                }
                default -> sender.sendMessage("harness money|balance|count|list|listings|expire|expiry|buy|price|open|sign|reset"
                        + "|state|live|cap|stall|snipe|intrude|kickon");
            }
        } catch (RuntimeException wrong) {
            sender.sendMessage("error " + wrong.getClass().getSimpleName() + ": " + wrong.getMessage());
        }
        return true;
    }
}
