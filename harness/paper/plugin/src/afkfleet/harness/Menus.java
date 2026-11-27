package afkfleet.harness;

import java.math.BigDecimal;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Optional;
import java.util.Set;
import java.util.TreeMap;
import java.util.UUID;

import io.papermc.paper.event.packet.UncheckedSignChangeEvent;
import io.papermc.paper.math.BlockPosition;
import io.papermc.paper.math.Position;

import net.kyori.adventure.text.Component;
import net.kyori.adventure.text.format.NamedTextColor;
import net.kyori.adventure.text.format.TextDecoration;
import net.kyori.adventure.text.serializer.plain.PlainTextComponentSerializer;
import org.bukkit.Bukkit;
import org.bukkit.Location;
import org.bukkit.Material;
import org.bukkit.block.sign.Side;
import org.bukkit.entity.Player;
import org.bukkit.event.inventory.InventoryClickEvent;
import org.bukkit.event.inventory.PrepareAnvilEvent;
import org.bukkit.event.inventory.InventoryCloseEvent;
import org.bukkit.event.inventory.InventoryDragEvent;
import org.bukkit.event.inventory.InventoryType;
import org.bukkit.inventory.Inventory;
import org.bukkit.inventory.InventoryHolder;
import org.bukkit.inventory.InventoryView;
import org.bukkit.inventory.ItemStack;
import org.bukkit.inventory.MenuType;
import org.bukkit.inventory.PlayerInventory;
import org.bukkit.inventory.meta.ItemMeta;
import org.bukkit.inventory.meta.PotionMeta;
import org.bukkit.inventory.view.AnvilView;

/**
 * The harness's menus, each a chest screen the plugin owns, as a server's auction and sell
 * plugins build them:
 *
 * <ul>
 *   <li><b>Auction House</b> (54 slots): listings in slots 0 to 44, each the item with its price,
 *       seller and time left added to its lore; previous page (45), Create (47), the page (48),
 *       expired listings (49), refresh (50) and next page (53). A page turns and a refresh redraw
 *       the same screen; a listing opens Confirm Purchase on the next tick. {@code /ah <query>}
 *       shows only the listings whose item, name or potion has the query in it.
 *   <li><b>The user's server's flow</b> (Plan.md P16.11, ADR-0036 M17): Create opens <b>Create
 *       Listing</b> (27 slots, cancel at 22), where a click on an item in the player's own inventory
 *       selects it; <b>Confirm Item</b> (confirm 11, the item 13, cancel 15); then an anvil the
 *       plugin opens, <b>Preis eingeben</b>, whose text box is the price (the user's correction of
 *       2026-11-26: a sign stood here before); then the final <b>Confirm Listing</b>. Each screen
 *       opens on the tick after its click.
 *   <li><b>Preis eingeben</b>, the price's anvil: paper named {@value #PRICE_START} in the left
 *       input, whose name is the text box's starting text; a lime dye named
 *       {@value #PRICE_CONFIRM} in the result slot (2), which confirms; an enchantment cost of 1
 *       shown, which needs no levels, as on the user's server. A click on the result reads the
 *       anvil's rename text: a whole number from 1 to {@value #MAX_PRICE} opens Confirm Listing,
 *       anything else is refused and the anvil stays open. Nothing of the anvil's goes back into
 *       the player's inventory when it closes.
 *   <li><b>Confirm Purchase</b> and <b>Confirm Listing</b> (27 slots): confirm (11), the item
 *       (13), cancel (15).
 *   <li><b>Expired Listings</b> (54 slots): the player's expired items in 0 to 44, back (49); a
 *       click collects an item into the inventory and redraws the screen.
 *   <li><b>Sell Items</b> (54 slots): slots 0 to 44 take any item, shift-clicks included; sell
 *       (49) sells what has a price and leaves the rest; closing gives back what's left.
 * </ul>
 *
 * Every click on a button is cancelled, as a plugin's are, and the server puts the item back.
 *
 * <p>The live tests' controls (Plan.md P16.17, P16.18) make each race happen on a known tick: the
 * other player buying first (a snipe), the market redrawn on every open auction house (live), a
 * price the anvil refuses (a cap), a confirm that never answers (a stall), a screen opened
 * unasked (an intrusion) and a kick inside a click.
 */
final class Menus {
    /** The auction house's listings per page. */
    static final int PAGE = 45;

    private final HarnessPlugin plugin;
    private final Market market;

    Menus(HarnessPlugin plugin, Market market) {
        this.plugin = plugin;
        this.market = market;
    }

    /** What a screen of the harness is, kept with its inventory. */
    abstract static class Screen implements InventoryHolder {
        Inventory inventory;

        @Override
        public Inventory getInventory() {
            return inventory;
        }
    }

    static final class AuctionScreen extends Screen {
        int page;
        /** What a search asked for; {@code null} for every listing. */
        String query;
        /** The listing in each slot as drawn. */
        final Map<Integer, Integer> listingAt = new java.util.HashMap<>();
    }

    static final class ConfirmPurchase extends Screen {
        int listing;
    }

    static final class ConfirmListing extends Screen {
        BigDecimal price;
        ItemStack inHand;
        int handSlot;
    }

    static final class ExpiredScreen extends Screen {}

    static final class CreateListing extends Screen {}

    static final class ConfirmItem extends Screen {
        ItemStack item;
        int slot;
    }

    /** The price anvil's title, as on the user's server. */
    static final String PRICE_TITLE = "Preis eingeben";
    /** The name of the price anvil's left input: the text box's starting text. */
    static final String PRICE_START = "Preis";
    /** The name of the price anvil's confirm item, in its result slot. */
    static final String PRICE_CONFIRM = "Bestätigen";
    /** The highest price the anvil takes. */
    static final long MAX_PRICE = 1_000_000_000L;

    /** A listing waiting for its price on the anvil the plugin opened: the view tells it apart. */
    record PriceAnvil(AnvilView view, ItemStack item, int slot) {}

    /** Each player's listing waiting for its price on an anvil. */
    private final Map<UUID, PriceAnvil> anvils = new HashMap<>();

    // --- The tests' controls (Plan.md P16.17, P16.18), set by `harness …` ------------------------

    /** Whether every open auction house is redrawn when the market changes, as most plugins do. */
    private boolean live;
    /** The highest price the price anvil takes: {@value #MAX_PRICE} unless the tests lower it. */
    private long priceCap = MAX_PRICE;
    /** Whether Confirm Item's confirm does nothing, as a plugin that hangs. */
    private boolean stalled;

    /**
     * A purchase armed for the next time another player opens a screen: on {@code house}, the
     * cheapest listing of {@code item} at {@code max} or less, the tick after they open the
     * auction house; on {@code confirm}, the listing they're about to confirm, the tick after
     * Confirm Purchase opens. The other player who buys first.
     */
    record Snipe(String buyer, String on, String item, BigDecimal max) {}

    private final List<Snipe> snipes = new ArrayList<>();

    /** A screen the plugin opens unasked for {@code player} the tick after they open {@code after}. */
    record Intrusion(String player, String after, String menu) {}

    private final List<Intrusion> intrusions = new ArrayList<>();

    /** The players kicked at their next click, inside it. */
    private final Set<String> kickAtClick = new HashSet<>();

    void setLive(boolean on) {
        live = on;
    }

    void setPriceCap(long cap) {
        priceCap = cap;
    }

    void setStalled(boolean on) {
        stalled = on;
    }

    void arm(Snipe snipe) {
        snipes.add(snipe);
    }

    void intrude(Intrusion intrusion) {
        intrusions.add(intrusion);
    }

    void kickAtClick(String player) {
        kickAtClick.add(player.toLowerCase(Locale.ROOT));
    }

    /** Whether {@code player}'s click is the one to kick them at, once. */
    boolean kicksAt(Player player) {
        return kickAtClick.remove(player.getName().toLowerCase(Locale.ROOT));
    }

    /** The market changed: every open auction house shows it at once when the harness is live. */
    void marketChanged() {
        if (!live) {
            return;
        }
        for (Player player : Bukkit.getOnlinePlayers()) {
            if (player.getOpenInventory().getTopInventory().getHolder() instanceof AuctionScreen screen) {
                drawAuction(screen);
            }
        }
    }

    /** The listing a click meets: an auction house's slot as drawn, or a Confirm Purchase's confirm. */
    String listingAt(InventoryClickEvent event) {
        InventoryHolder holder = event.getView().getTopInventory().getHolder();
        Integer id = switch (holder) {
            case AuctionScreen screen -> screen.listingAt.get(event.getRawSlot());
            case ConfirmPurchase screen -> event.getRawSlot() == 11 ? screen.listing : null;
            case null, default -> null;
        };
        return id == null ? "-" : id.toString();
    }

    /** The listing a screen shows for purchase: a Confirm Purchase's. */
    String listingOf(InventoryView view) {
        return view.getTopInventory().getHolder() instanceof ConfirmPurchase screen
                ? Integer.toString(screen.listing) : "-";
    }

    /** {@code player} opened a screen titled {@code title}: a snipe or an intrusion may follow. */
    void opened(Player player, String title, InventoryView view) {
        InventoryHolder holder = view.getTopInventory().getHolder();
        String name = player.getName();
        for (java.util.Iterator<Snipe> armed = snipes.iterator(); armed.hasNext();) {
            Snipe snipe = armed.next();
            if (snipe.buyer().equalsIgnoreCase(name)) {
                continue;
            }
            Optional<Integer> target = switch (holder) {
                case AuctionScreen screen when snipe.on().equals("house") -> cheapest(snipe.item(), snipe.max());
                case ConfirmPurchase screen when snipe.on().equals("confirm") -> Optional.of(screen.listing);
                case null, default -> null;
            };
            if (target == null) {
                continue;
            }
            armed.remove();
            Bukkit.getScheduler().runTask(plugin, () -> {
                Player buyer = Bukkit.getPlayerExact(snipe.buyer());
                if (buyer == null || target.isEmpty()) {
                    plugin.log("harness-snipe buyer=" + snipe.buyer() + " on=" + snipe.on() + " id=none");
                    return;
                }
                plugin.log("harness-snipe buyer=" + snipe.buyer() + " on=" + snipe.on() + " id=" + target.get());
                plugin.purchase(buyer, target.get());
            });
            break;
        }
        for (java.util.Iterator<Intrusion> armed = intrusions.iterator(); armed.hasNext();) {
            Intrusion intrusion = armed.next();
            if (!intrusion.player().equalsIgnoreCase(name) || !intrusion.after().equals(title)) {
                continue;
            }
            armed.remove();
            Bukkit.getScheduler().runTask(plugin, () -> {
                plugin.log("harness-intrude player=" + name + " after=\"" + title + "\" menu=" + intrusion.menu());
                open(player, intrusion.menu());
            });
            break;
        }
    }

    /** The cheapest listing of {@code item} (its key as {@link Market#sellKey} has it) at {@code max} or less. */
    private Optional<Integer> cheapest(String item, BigDecimal max) {
        return market.listings().stream()
                .filter(listing -> Market.sellKey(listing.item()).equals(item))
                .filter(listing -> listing.price().compareTo(max) <= 0)
                .min(java.util.Comparator.comparing(Market.Listing::price))
                .map(Market.Listing::id);
    }

    /** Opens the menu {@code menu} (ah, sell or expired) for {@code player}; false for another name. */
    boolean open(Player player, String menu) {
        switch (menu) {
            case "ah" -> openAuction(player, 0);
            case "sell" -> openSell(player);
            case "expired" -> openExpired(player);
            default -> {
                return false;
            }
        }
        return true;
    }

    /**
     * What the server has of {@code player}: the screen open (none for their own inventory), the
     * cursor, the held slot (1 to 9) and item, whether a hand is raised (an item in use, a shield
     * up), sneaking and sprinting, the effects, and every item they carry by its key and count.
     */
    String state(Player player) {
        InventoryView view = player.getOpenInventory();
        String screen = view.getTopInventory().getType() == InventoryType.CRAFTING
                ? "none" : '"' + PlainTextComponentSerializer.plainText().serialize(view.title()) + '"';
        ItemStack cursor = view.getCursor();
        PlayerInventory inventory = player.getInventory();
        ItemStack held = inventory.getItemInMainHand();
        Map<String, Integer> items = new TreeMap<>();
        for (ItemStack item : inventory.getContents()) {
            if (item != null && !item.getType().isAir()) {
                items.merge(Market.sellKey(item), item.getAmount(), Integer::sum);
            }
        }
        List<String> effects = player.getActivePotionEffects().stream()
                .map(effect -> effect.getType().getKey().toString()).sorted().toList();
        return "state player=" + player.getName() + " screen=" + screen
                + " cursor=" + (cursor.getType().isAir() ? "none" : Market.sellKey(cursor))
                + " held=" + (inventory.getHeldItemSlot() + 1)
                + " item=" + (held.getType().isAir() ? "none" : Market.sellKey(held))
                + " raised=" + player.isHandRaised() + " sneaking=" + player.isSneaking()
                + " sprinting=" + player.isSprinting() + " effects=" + effects + " items=" + items;
    }

    static final class SellScreen extends Screen {}

    // --- Opening -------------------------------------------------------------------------------

    void openAuction(Player player, int page) {
        openAuction(player, page, null);
    }

    /** The auction house, only with the listings that match {@code query} when it's set. */
    void openAuction(Player player, int page, String query) {
        AuctionScreen screen = new AuctionScreen();
        screen.inventory = Bukkit.createInventory(screen, 54, Component.text("Auction House"));
        screen.page = page;
        screen.query = query;
        drawAuction(screen);
        player.openInventory(screen.inventory);
    }

    private void openCreate(Player player) {
        CreateListing screen = new CreateListing();
        screen.inventory = Bukkit.createInventory(screen, 27, Component.text("Create Listing"));
        for (int slot = 0; slot < 27; slot++) {
            screen.inventory.setItem(slot, filler());
        }
        screen.inventory.setItem(13, button(Material.PAPER, "Select an item", NamedTextColor.YELLOW,
                "Click an item in your inventory"));
        screen.inventory.setItem(22, button(Material.RED_STAINED_GLASS_PANE, "Cancel", NamedTextColor.RED));
        player.openInventory(screen.inventory);
    }

    private void openConfirmItem(Player player, ItemStack item, int slot) {
        ConfirmItem screen = new ConfirmItem();
        screen.inventory = Bukkit.createInventory(screen, 27, Component.text("Confirm Item"));
        screen.item = item;
        screen.slot = slot;
        drawConfirm(screen.inventory, item);
        player.openInventory(screen.inventory);
    }

    /**
     * Opens the price's anvil for a listing of {@code item}, which is in slot {@code slot} of the
     * player's inventory: anywhere, with no anvil block near.
     */
    private void openPriceAnvil(Player player, ItemStack item, int slot) {
        AnvilView view = MenuType.ANVIL.builder()
                .title(Component.text(PRICE_TITLE))
                .checkReachable(false)
                .build(player);
        anvils.put(player.getUniqueId(), new PriceAnvil(view, item, slot));
        player.openInventory(view);
        view.getTopInventory().setItem(0, named(Material.PAPER, PRICE_START));
        view.getTopInventory().setItem(2, priceConfirm());
        view.setRepairCost(1);
        plugin.log("harness-anvil-open player=" + player.getName() + " level=" + player.getLevel());
    }

    /** The price anvil's confirm item. */
    private static ItemStack priceConfirm() {
        return button(Material.LIME_DYE, PRICE_CONFIRM, NamedTextColor.GREEN);
    }

    /** An item named {@code name}, nothing else. */
    private static ItemStack named(Material material, String name) {
        ItemStack item = new ItemStack(material);
        ItemMeta meta = item.getItemMeta();
        meta.displayName(Component.text(name).decoration(TextDecoration.ITALIC, false));
        item.setItemMeta(meta);
        return item;
    }

    /** The price anvil of {@code view}'s player, if {@code view} is it. */
    private PriceAnvil priceAnvil(InventoryView view, UUID player) {
        PriceAnvil anvil = anvils.get(player);
        return anvil != null && anvil.view() == view ? anvil : null;
    }

    /** Opens a sign editor for {@code player} at a place below their feet; nothing is built there. */
    BlockPosition openSign(Player player) {
        Location at = player.getLocation();
        BlockPosition sign = Position.block(at.getBlockX(), at.getBlockY() - 3, at.getBlockZ());
        player.openVirtualSign(sign, Side.FRONT);
        plugin.log("harness-sign-open player=" + player.getName() + " at=" + sign.blockX() + ","
                + sign.blockY() + "," + sign.blockZ());
        return sign;
    }

    void openExpired(Player player) {
        ExpiredScreen screen = new ExpiredScreen();
        screen.inventory = Bukkit.createInventory(screen, 54, Component.text("Expired Listings"));
        drawExpired(screen, player);
        player.openInventory(screen.inventory);
    }

    void openSell(Player player) {
        SellScreen screen = new SellScreen();
        screen.inventory = Bukkit.createInventory(screen, 54, Component.text("Sell Items"));
        for (int slot = PAGE; slot < 54; slot++) {
            screen.inventory.setItem(slot, filler());
        }
        screen.inventory.setItem(49, button(Material.EMERALD, "Sell", NamedTextColor.GREEN,
                "Sells every item above that has a price"));
        player.openInventory(screen.inventory);
    }

    /** {@code /ah sell <price>}: the item in the player's hand, to be confirmed. */
    void openConfirmListing(Player player, BigDecimal price) {
        ItemStack hand = player.getInventory().getItemInMainHand();
        if (hand.getType().isAir()) {
            player.sendMessage(Component.text("Hold the item you want to sell.", NamedTextColor.RED));
            return;
        }
        openConfirmListing(player, price, hand.clone(), player.getInventory().getHeldItemSlot());
    }

    /** The final confirm of a listing of {@code item}, which is in slot {@code slot} of the inventory. */
    private void openConfirmListing(Player player, BigDecimal price, ItemStack item, int slot) {
        ConfirmListing screen = new ConfirmListing();
        screen.inventory = Bukkit.createInventory(screen, 27, Component.text("Confirm Listing"));
        screen.price = price;
        screen.inHand = item;
        screen.handSlot = slot;
        drawConfirm(screen.inventory, withLore(item, List.of(
                Component.text("Price: " + Market.money(price), NamedTextColor.GOLD))));
        player.openInventory(screen.inventory);
    }

    private void openConfirmPurchase(Player player, Market.Listing listing) {
        ConfirmPurchase screen = new ConfirmPurchase();
        screen.inventory = Bukkit.createInventory(screen, 27, Component.text("Confirm Purchase"));
        screen.listing = listing.id();
        drawConfirm(screen.inventory, shown(listing));
        player.openInventory(screen.inventory);
    }

    // --- Drawing -------------------------------------------------------------------------------

    private void drawAuction(AuctionScreen screen) {
        Inventory inventory = screen.inventory;
        inventory.clear();
        screen.listingAt.clear();
        List<Market.Listing> listings = market.listings().stream()
                .filter(listing -> screen.query == null || matches(listing.item(), screen.query))
                .toList();
        int pages = Math.max(1, (listings.size() + PAGE - 1) / PAGE);
        screen.page = Math.max(0, Math.min(screen.page, pages - 1));
        int first = screen.page * PAGE;
        for (int slot = 0; slot < PAGE && first + slot < listings.size(); slot++) {
            Market.Listing listing = listings.get(first + slot);
            inventory.setItem(slot, shown(listing));
            screen.listingAt.put(slot, listing.id());
        }
        for (int slot = PAGE; slot < 54; slot++) {
            inventory.setItem(slot, filler());
        }
        if (screen.page > 0) {
            inventory.setItem(45, button(Material.ARROW, "Previous page", NamedTextColor.YELLOW));
        }
        inventory.setItem(47, button(Material.EMERALD, "Create", NamedTextColor.GREEN,
                "Start a new listing"));
        inventory.setItem(48, button(Material.PAPER, "Page " + (screen.page + 1) + " of " + pages,
                NamedTextColor.WHITE));
        Player viewer = viewer(inventory);
        int expired = viewer == null ? 0 : market.expired(viewer.getName()).size();
        inventory.setItem(49, button(Material.CHEST, "Expired listings", NamedTextColor.YELLOW,
                expired + " to collect"));
        inventory.setItem(50, button(Material.SUNFLOWER, "Refresh", NamedTextColor.YELLOW));
        if (screen.page < pages - 1) {
            inventory.setItem(53, button(Material.ARROW, "Next page", NamedTextColor.YELLOW));
        }
    }

    /** Whether {@code item}'s kind, its name or its potion has {@code query} in it, in any case. */
    private static boolean matches(ItemStack item, String query) {
        String wanted = query.toLowerCase(Locale.ROOT);
        StringBuilder text = new StringBuilder(item.getType().getKey().toString());
        text.append(' ').append(PlainTextComponentSerializer.plainText().serialize(item.effectiveName()));
        if (item.getItemMeta() instanceof PotionMeta potion && potion.hasBasePotionType()) {
            text.append(' ').append(potion.getBasePotionType().getKey());
        }
        return text.toString().toLowerCase(Locale.ROOT).contains(wanted);
    }

    private void drawExpired(ExpiredScreen screen, Player player) {
        Inventory inventory = screen.inventory;
        inventory.clear();
        List<ItemStack> items = market.expired(player.getName());
        for (int slot = 0; slot < PAGE && slot < items.size(); slot++) {
            inventory.setItem(slot, withLore(items.get(slot), List.of(
                    Component.text("Click to collect", NamedTextColor.GRAY))));
        }
        for (int slot = PAGE; slot < 54; slot++) {
            inventory.setItem(slot, filler());
        }
        inventory.setItem(49, button(Material.ARROW, "Back", NamedTextColor.YELLOW));
    }

    private static void drawConfirm(Inventory inventory, ItemStack item) {
        for (int slot = 0; slot < 27; slot++) {
            inventory.setItem(slot, filler());
        }
        inventory.setItem(11, button(Material.LIME_STAINED_GLASS_PANE, "Confirm", NamedTextColor.GREEN));
        inventory.setItem(13, item);
        inventory.setItem(15, button(Material.RED_STAINED_GLASS_PANE, "Cancel", NamedTextColor.RED));
    }

    /** A listing as the auction house shows it: the item, and below its own lore the sale. */
    private ItemStack shown(Market.Listing listing) {
        long ticksLeft = Math.max(0, listing.expiresAtTick() - Bukkit.getCurrentTick());
        long minutes = ticksLeft / 20 / 60;
        long seconds = ticksLeft / 20 % 60;
        return withLore(listing.item(), List.of(
                Component.text("Price: " + Market.money(listing.price()), NamedTextColor.GOLD),
                Component.text("Seller: " + listing.seller(), NamedTextColor.GRAY),
                Component.text("Expires in: " + minutes + "m " + seconds + "s", NamedTextColor.GRAY)));
    }

    private static ItemStack withLore(ItemStack item, List<Component> added) {
        ItemStack copy = item.clone();
        ItemMeta meta = copy.getItemMeta();
        List<Component> lore = new ArrayList<>();
        if (meta.lore() != null) {
            lore.addAll(meta.lore());
        }
        for (Component line : added) {
            lore.add(line.decoration(TextDecoration.ITALIC, false));
        }
        meta.lore(lore);
        copy.setItemMeta(meta);
        return copy;
    }

    private static ItemStack button(Material material, String name, NamedTextColor color, String... lore) {
        ItemStack item = new ItemStack(material);
        ItemMeta meta = item.getItemMeta();
        meta.displayName(Component.text(name, color).decoration(TextDecoration.ITALIC, false));
        List<Component> lines = new ArrayList<>();
        for (String line : lore) {
            lines.add(Component.text(line, NamedTextColor.GRAY).decoration(TextDecoration.ITALIC, false));
        }
        if (!lines.isEmpty()) {
            meta.lore(lines);
        }
        item.setItemMeta(meta);
        return item;
    }

    private static ItemStack filler() {
        return button(Material.BLACK_STAINED_GLASS_PANE, " ", NamedTextColor.DARK_GRAY);
    }

    private static Player viewer(Inventory inventory) {
        return inventory.getViewers().isEmpty() ? null : (Player) inventory.getViewers().get(0);
    }

    // --- Clicks --------------------------------------------------------------------------------

    /** Whether {@code holder} is one of the harness's screens. */
    static boolean ours(InventoryHolder holder) {
        return holder instanceof Screen;
    }

    void onClick(InventoryClickEvent event) {
        if (event.getWhoClicked() instanceof Player clicker) {
            PriceAnvil anvil = priceAnvil(event.getView(), clicker.getUniqueId());
            if (anvil != null) {
                // Nothing moves in it: every click is the plugin's, the result's a confirm.
                event.setCancelled(true);
                if (event.getRawSlot() == 2) {
                    clickPrice(clicker, anvil);
                }
                return;
            }
        }
        InventoryHolder holder = event.getView().getTopInventory().getHolder();
        if (!(holder instanceof Screen) || !(event.getWhoClicked() instanceof Player player)) {
            return;
        }
        int raw = event.getRawSlot();
        boolean top = raw >= 0 && raw < event.getView().getTopInventory().getSize();
        if (holder instanceof SellScreen) {
            // Slots 0 to 44 and the player's own take items; the bottom row is buttons.
            if (top && raw >= PAGE) {
                event.setCancelled(true);
                if (raw == 49) {
                    sell(player, event.getView().getTopInventory());
                }
            }
            return;
        }
        event.setCancelled(true);
        if (holder instanceof CreateListing && !top && raw >= 0) {
            // The player's own inventory: the item clicked is the one to list.
            ItemStack clicked = event.getCurrentItem();
            if (clicked == null || clicked.getType().isAir()) {
                return;
            }
            ItemStack item = clicked.clone();
            int slot = event.getSlot();
            Bukkit.getScheduler().runTask(plugin, () -> openConfirmItem(player, item, slot));
            return;
        }
        if (!top) {
            return;
        }
        switch (holder) {
            case AuctionScreen screen -> clickAuction(player, screen, raw);
            case ConfirmPurchase screen -> clickConfirmPurchase(player, screen, raw);
            case ConfirmListing screen -> clickConfirmListing(player, screen, raw);
            case ExpiredScreen screen -> clickExpired(player, screen, raw);
            case CreateListing screen -> {
                if (raw == 22) {
                    Bukkit.getScheduler().runTask(plugin, () -> player.closeInventory());
                }
            }
            case ConfirmItem screen -> clickConfirmItem(player, screen, raw);
            default -> {}
        }
    }

    void onDrag(InventoryDragEvent event) {
        if (priceAnvil(event.getView(), event.getWhoClicked().getUniqueId()) != null) {
            event.setCancelled(true);
            return;
        }
        InventoryHolder holder = event.getView().getTopInventory().getHolder();
        if (!(holder instanceof Screen)) {
            return;
        }
        int size = event.getView().getTopInventory().getSize();
        boolean intoButtons = event.getRawSlots().stream()
                .anyMatch(slot -> slot < size && (!(holder instanceof SellScreen) || slot >= PAGE));
        if (intoButtons) {
            event.setCancelled(true);
        }
    }

    void onClose(InventoryCloseEvent event) {
        PriceAnvil anvil = priceAnvil(event.getView(), event.getPlayer().getUniqueId());
        if (anvil != null) {
            // The anvil's paper is the plugin's: it never goes into the player's inventory.
            anvil.view().getTopInventory().clear();
            anvils.remove(event.getPlayer().getUniqueId());
            return;
        }
        if (event.getView().getTopInventory().getHolder() instanceof SellScreen
                && event.getPlayer() instanceof Player player) {
            Inventory top = event.getView().getTopInventory();
            for (int slot = 0; slot < PAGE; slot++) {
                ItemStack item = top.getItem(slot);
                if (item != null && !item.getType().isAir()) {
                    top.setItem(slot, null);
                    giveOrDrop(player, item);
                }
            }
        }
    }

    private void clickAuction(Player player, AuctionScreen screen, int slot) {
        Integer id = screen.listingAt.get(slot);
        if (id != null) {
            Optional<Market.Listing> listing = market.listing(id);
            if (listing.isEmpty()) {
                plugin.log("harness-gone player=" + player.getName() + " id=" + id);
                player.sendMessage(Component.text(Market.Refusal.GONE.text, NamedTextColor.RED));
                drawAuction(screen);
                return;
            }
            // As most plugins do: never open a screen inside the click's own event.
            Bukkit.getScheduler().runTask(plugin, () -> openConfirmPurchase(player, listing.get()));
            return;
        }
        switch (slot) {
            case 45 -> {
                screen.page--;
                drawAuction(screen);
            }
            case 53 -> {
                screen.page++;
                drawAuction(screen);
            }
            case 50 -> drawAuction(screen);
            case 47 -> Bukkit.getScheduler().runTask(plugin, () -> openCreate(player));
            case 49 -> Bukkit.getScheduler().runTask(plugin, () -> openExpired(player));
            default -> {}
        }
    }

    private void clickConfirmItem(Player player, ConfirmItem screen, int slot) {
        if (slot == 11 && stalled) {
            plugin.log("harness-stalled player=" + player.getName());
        } else if (slot == 11) {
            Bukkit.getScheduler().runTask(plugin, () -> openPriceAnvil(player, screen.item, screen.slot));
        } else if (slot == 15) {
            Bukkit.getScheduler().runTask(plugin, () -> player.closeInventory());
        }
    }

    /**
     * The price anvil's result was clicked: its rename text is the price. A whole number from 1
     * to {@value #MAX_PRICE} opens Confirm Listing on the next tick; anything else is refused, and
     * the anvil stays open.
     */
    private void clickPrice(Player player, PriceAnvil anvil) {
        String text = anvil.view().getRenameText();
        String typed = text == null ? "" : text.trim();
        Optional<Long> price = wholePrice(typed).filter(whole -> whole <= priceCap);
        plugin.log("harness-anvil player=" + player.getName() + " text=" + typed + " level=" + player.getLevel()
                + " taken=" + price.isPresent());
        if (price.isEmpty()) {
            player.sendMessage(Component.text("That's not a price.", NamedTextColor.RED));
            return;
        }
        anvils.remove(player.getUniqueId());
        Bukkit.getScheduler().runTask(plugin,
                () -> openConfirmListing(player, BigDecimal.valueOf(price.get()), anvil.item(), anvil.slot()));
    }

    /** {@code text} as a whole price from 1 to {@value #MAX_PRICE}, or none. */
    static Optional<Long> wholePrice(String text) {
        if (text.isEmpty() || text.length() > 10 || !text.chars().allMatch(c -> c >= '0' && c <= '9')) {
            return Optional.empty();
        }
        long price = Long.parseLong(text);
        return price >= 1 && price <= MAX_PRICE ? Optional.of(price) : Optional.empty();
    }

    /**
     * The anvil's result is the plugin's confirm item whatever is typed, at the cost the user's
     * server shows; the server would compute a renamed paper there.
     */
    void onPrepareAnvil(PrepareAnvilEvent event) {
        if (priceAnvil(event.getView(), event.getView().getPlayer().getUniqueId()) == null) {
            return;
        }
        event.setResult(priceConfirm());
        event.getView().setRepairCost(1);
    }

    /**
     * A sign's lines came back. Every sign the plugin opens is virtual ({@code harness sign}), so
     * each comes here and is only logged: no flow of the harness takes a price on a sign since the
     * user's correction of 2026-11-26, but other servers do, and the bot's sign editor is tested
     * here.
     */
    void onSign(UncheckedSignChangeEvent event) {
        event.setCancelled(true);
        Player player = event.getPlayer();
        List<String> lines = event.lines().stream()
                .map(line -> PlainTextComponentSerializer.plainText().serialize(line))
                .toList();
        plugin.log("harness-sign player=" + player.getName() + " lines=" + lines);
    }

    private void clickConfirmPurchase(Player player, ConfirmPurchase screen, int slot) {
        if (slot == 11) {
            plugin.purchase(player, screen.listing);
            Bukkit.getScheduler().runTask(plugin, () -> player.closeInventory());
        } else if (slot == 15) {
            Bukkit.getScheduler().runTask(plugin, () -> openAuction(player, 0));
        }
    }

    private void clickConfirmListing(Player player, ConfirmListing screen, int slot) {
        if (slot == 11) {
            ItemStack hand = player.getInventory().getItem(screen.handSlot);
            if (hand == null || !hand.isSimilar(screen.inHand) || hand.getAmount() != screen.inHand.getAmount()) {
                player.sendMessage(Component.text("The item in your hand changed.", NamedTextColor.RED));
            } else {
                player.getInventory().setItem(screen.handSlot, null);
                Market.Listing listing = market.list(player.getName(), screen.inHand, screen.price,
                        Bukkit.getCurrentTick());
                plugin.getLogger().info("harness-listed id=" + listing.id() + " seller=" + player.getName()
                        + " price=" + listing.price().toPlainString());
                marketChanged();
                player.sendMessage(Component.text("Listed for " + Market.money(screen.price) + ".",
                        NamedTextColor.GREEN));
            }
            Bukkit.getScheduler().runTask(plugin, () -> player.closeInventory());
        } else if (slot == 15) {
            Bukkit.getScheduler().runTask(plugin, () -> player.closeInventory());
        }
    }

    private void clickExpired(Player player, ExpiredScreen screen, int slot) {
        if (slot == 49) {
            Bukkit.getScheduler().runTask(plugin, () -> openAuction(player, 0));
            return;
        }
        List<ItemStack> items = market.expired(player.getName());
        if (slot >= items.size()) {
            return;
        }
        ItemStack item = items.get(slot);
        if (!player.getInventory().addItem(item.clone()).isEmpty()) {
            // addItem may have placed part of it: take that back.
            player.getInventory().removeItem(item.clone());
            player.sendMessage(Component.text(Market.Refusal.FULL.text, NamedTextColor.RED));
            return;
        }
        items.remove(slot);
        plugin.getLogger().info("harness-collected player=" + player.getName());
        drawExpired(screen, player);
    }

    private void sell(Player player, Inventory top) {
        BigDecimal total = BigDecimal.ZERO;
        int sold = 0;
        for (int slot = 0; slot < PAGE; slot++) {
            ItemStack item = top.getItem(slot);
            if (item == null || item.getType().isAir()) {
                continue;
            }
            Optional<BigDecimal> price = market.sellPrice(item);
            if (price.isPresent()) {
                total = total.add(price.get().multiply(BigDecimal.valueOf(item.getAmount())));
                sold += item.getAmount();
                top.setItem(slot, null);
            }
        }
        if (sold == 0) {
            player.sendMessage(Component.text("Nothing here sells.", NamedTextColor.RED));
            return;
        }
        market.setBalance(player.getName(), market.balance(player.getName()).add(total));
        plugin.getLogger().info("harness-sold player=" + player.getName() + " items=" + sold
                + " total=" + total.toPlainString());
        player.sendMessage(Component.text("Sold " + sold + " items for " + Market.money(total) + ".",
                NamedTextColor.GREEN));
    }

    static void giveOrDrop(Player player, ItemStack item) {
        for (ItemStack left : player.getInventory().addItem(item).values()) {
            player.getWorld().dropItem(player.getLocation(), left);
        }
    }
}
