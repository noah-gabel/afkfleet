package afkfleet.harness;

import java.math.BigDecimal;
import java.util.Arrays;
import java.util.Optional;

import io.papermc.paper.event.packet.UncheckedSignChangeEvent;
import net.kyori.adventure.text.Component;
import net.kyori.adventure.text.format.NamedTextColor;
import net.kyori.adventure.text.serializer.plain.PlainTextComponentSerializer;
import org.bukkit.Bukkit;
import org.bukkit.command.Command;
import org.bukkit.command.CommandSender;
import org.bukkit.command.ConsoleCommandSender;
import org.bukkit.command.RemoteConsoleCommandSender;
import org.bukkit.entity.Player;
import org.bukkit.event.EventHandler;
import org.bukkit.event.EventPriority;
import org.bukkit.event.Listener;
import org.bukkit.event.inventory.InventoryClickEvent;
import org.bukkit.event.inventory.InventoryCloseEvent;
import org.bukkit.event.inventory.InventoryDragEvent;
import org.bukkit.event.inventory.InventoryOpenEvent;
import org.bukkit.event.player.PlayerKickEvent;
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
 * harness-collected}, {@code harness-sold}, {@code harness-sign-open} and {@code harness-sign}.
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

    /** Every click, after every handler, with whether it was cancelled. */
    @EventHandler(priority = EventPriority.MONITOR)
    public void logClick(InventoryClickEvent event) {
        log("harness-click player=" + event.getWhoClicked().getName() + " screen=" + title(event.getView())
                + " raw=" + event.getRawSlot() + " slot=" + event.getSlot() + " click=" + event.getClick()
                + " action=" + event.getAction() + " hotbar=" + event.getHotbarButton()
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
                + " cancelled=" + event.isCancelled());
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

    @EventHandler
    public void onDrag(InventoryDragEvent event) {
        menus.onDrag(event);
    }

    @EventHandler
    public void onClose(InventoryCloseEvent event) {
        menus.onClose(event);
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
     * {@code sign <player>} (a sign editor nobody asked for) and {@code reset}.
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
                case "list" -> {
                    ItemStack item = Bukkit.getItemFactory().createItemStack(
                            String.join(" ", Arrays.copyOfRange(args, 3, args.length)));
                    BigDecimal price = Market.parsePrice(args[2])
                            .orElseThrow(() -> new IllegalArgumentException("not a price: " + args[2]));
                    Market.Listing listing = market.list(args[1], item, price, Bukkit.getCurrentTick());
                    log("harness-listed id=" + listing.id() + " seller=" + args[1] + " price="
                            + listing.price().toPlainString());
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
                case "expire" -> sender.sendMessage(market.expire(Integer.parseInt(args[1])) ? "expired" : "no such listing");
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
                        switch (args[2]) {
                            case "ah" -> menus.openAuction(player, 0);
                            case "sell" -> menus.openSell(player);
                            case "expired" -> menus.openExpired(player);
                            default -> sender.sendMessage("no such menu");
                        }
                        sender.sendMessage("opened");
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
                    sender.sendMessage("reset");
                }
                default -> sender.sendMessage("harness money|balance|list|listings|expire|expiry|buy|price|open|sign|reset");
            }
        } catch (RuntimeException wrong) {
            sender.sendMessage("error " + wrong.getClass().getSimpleName() + ": " + wrong.getMessage());
        }
        return true;
    }
}
