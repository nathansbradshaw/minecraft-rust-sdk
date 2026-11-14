package dev.minecraftsdk;

import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.tags.FluidTags;
import net.minecraft.server.MinecraftServer;
import net.minecraft.server.level.ServerPlayer;
import net.minecraft.world.item.ItemStack;
import net.minecraft.world.entity.LivingEntity;
import net.minecraft.world.entity.Mob;
import net.minecraft.world.entity.MobCategory;
import net.minecraft.world.entity.monster.Enemy;
import net.minecraft.world.entity.monster.Creeper;
import net.minecraft.world.entity.monster.RangedAttackMob;
import net.minecraft.world.entity.projectile.Projectile;
import net.minecraft.world.entity.item.ItemEntity;
import net.minecraft.world.entity.ai.attributes.Attributes;
import net.minecraft.world.effect.MobEffects;
import java.util.*;

/** Bounded observations, always collected together on the authoritative server thread. */
final class NativeObservation {
    static final int ENTITY_RADIUS=32;
    private static final String CLOCK_EPOCH=UUID.randomUUID().toString();
    private static final Set<String> RANGED_ANIMATIONS=Set.of("BOW","CROSSBOW","SPEAR");
    private static Map<String,Object> equipment(ItemStack item){return Map.of("item",BuiltInRegistries.ITEM.getKey(item.getItem()).toString(),"use_animation",item.getUseAnimation().name());}
    // Called only for the radius-bounded, line-of-sight-filtered entity rows.
    // Item-use animation is current observable state, not the mob's AI target
    // or a prediction that a shot will actually be released.
    private static Map<String,Object> visibleItemUse(LivingEntity living){
        boolean using=living.isUsingItem();var row=new LinkedHashMap<String,Object>();
        row.put("format","java767-visible-item-use-v1");row.put("using",using);
        row.put("item",equipment(living.getUseItem()));
        row.put("elapsed_ticks",using?Math.max(0,living.getTicksUsingItem()):0);
        row.put("remaining_ticks",using?Math.max(0,living.getUseItemRemainingTicks()):0);
        if(using)row.put("hand",living.getUsedItemHand().name());
        return row;
    }
    private static Map<String,Object> stack(ItemStack stack,ServerPlayer player){
        var result=new LinkedHashMap<String,Object>();result.put("item",BuiltInRegistries.ITEM.getKey(stack.getItem()).toString());result.put("count",stack.getCount());
        result.put("components",stack.isEmpty()?"":stack.save(player.registryAccess()).toString());
        if(!stack.isEmpty()){
            var food=stack.getFoodProperties(player);
            if(food!=null)result.put("food",Map.of("nutrition",food.nutrition(),"saturation",food.saturation(),"can_always_eat",food.canAlwaysEat(),"can_eat",player.canEat(food.canAlwaysEat())));
            result.put("use_duration_ticks",stack.getUseDuration(player));result.put("use_animation",stack.getUseAnimation().name());
            result.put("on_cooldown",player.getCooldowns().isOnCooldown(stack.getItem()));
        }
        return result;
    }
    static Object snapshot(MinecraftServer server,ServerPlayer player){
        if(!server.isSameThread())throw new IllegalStateException("Main server thread required");
        var life=LifeEpochs.of(player.connection.getConnection(),player);
        // Clock comparisons are valid within one native player life. A new
        // life invalidates cached physics, menus, targets and old effect proofs
        // even when a client never observed the brief dead interval.
        var sampleClock=Map.of("epoch",CLOCK_EPOCH+":"+life,"nanos",Long.toUnsignedString(System.nanoTime()));
        var world=player.serverLevel();var origin=player.blockPosition();var menu=player.containerMenu;
        var inventory=new ArrayList<Map<String,Object>>();
        for(int i=0;i<player.getInventory().getContainerSize();i++){var row=new LinkedHashMap<>(stack(player.getInventory().getItem(i),player));row.put("slot",i);inventory.add(row);}
        var slots=new ArrayList<Map<String,Object>>();
        if(menu.slots.size()>512)throw new IllegalStateException("Menu observation capability unavailable for more than 512 slots");
        for(var slot:menu.slots){var row=new LinkedHashMap<>(stack(slot.getItem(),player));row.put("slot",slot.index);row.put("inventory_slot",slot.getContainerSlot());row.put("player_inventory",slot.container==player.getInventory());row.put("may_pickup",slot.mayPickup(player));row.put("may_place_carried",slot.mayPlace(menu.getCarried()));slots.add(row);}
        var blocks=new ArrayList<Map<String,Object>>();var unknown=new ArrayList<List<Integer>>();
        for(int x=-4;x<=4;x++)for(int z=-4;z<=4;z++){
            if(!world.hasChunkAt(origin.offset(x,0,z))){unknown.add(List.of(origin.getX()+x,origin.getZ()+z));continue;}
            for(int y=-4;y<=4;y++){
                var pos=origin.offset(x,y,z);var state=world.getBlockState(pos);var row=new LinkedHashMap<String,Object>();
                row.put("position",List.of(pos.getX(),pos.getY(),pos.getZ()));row.put("block",BuiltInRegistries.BLOCK.getKey(state.getBlock()).toString());row.put("state",state.toString());row.put("replaceable",state.canBeReplaced());row.put("may_interact",world.mayInteract(player,pos));
                float progress=state.getDestroyProgress(player,world,pos);row.put("destroy_progress",progress>0?Math.min(1.0f,progress):0f);
                row.put("friction",state.getBlock().getFriction());row.put("speed_factor",state.getBlock().getSpeedFactor());
                var fluid=state.getFluidState();var fluidRow=new LinkedHashMap<String,Object>();
                fluidRow.put("format","java767-fluid-v1");fluidRow.put("empty",fluid.isEmpty());
                fluidRow.put("id",BuiltInRegistries.FLUID.getKey(fluid.getType()).toString());
                fluidRow.put("water",fluid.is(FluidTags.WATER));fluidRow.put("lava",fluid.is(FluidTags.LAVA));
                fluidRow.put("height",fluid.isEmpty()?0f:fluid.getHeight(world,pos));
                // Flow can inspect horizontal neighbours. Do not cause a chunk
                // load just to predict movement at the edge of a loaded scene.
                boolean flowKnown=fluid.isEmpty()||(world.hasChunkAt(pos.north())&&world.hasChunkAt(pos.south())&&world.hasChunkAt(pos.east())&&world.hasChunkAt(pos.west()));
                fluidRow.put("flow_known",flowKnown);
                if(flowKnown){var flow=fluid.isEmpty()?net.minecraft.world.phys.Vec3.ZERO:fluid.getFlow(world,pos);fluidRow.put("flow",List.of(flow.x,flow.y,flow.z));}
                row.put("fluid",fluidRow);
                row.put("collision",state.getCollisionShape(world,pos).toAabbs().stream().map(a->List.of(a.minX,a.minY,a.minZ,a.maxX,a.maxY,a.maxZ)).toList());blocks.add(row);
            }
        }
        var entities=new ArrayList<Map<String,Object>>();
        var visible=world.getEntities(player,player.getBoundingBox().inflate(ENTITY_RADIUS)).stream().filter(e->e.isAlive()&&player.hasLineOfSight(e))
            .sorted(Comparator.<net.minecraft.world.entity.Entity>comparingInt(e->e instanceof Enemy||e.getType().getCategory()==MobCategory.MONSTER||e instanceof Projectile&&e.getDeltaMovement().lengthSqr()>0.0001?0:1)
            .thenComparingDouble(e->e.distanceToSqr(player))).limit(128).toList();
        for(var entity:visible){var row=new LinkedHashMap<String,Object>();row.put("entity_id",entity.getId());row.put("uuid",entity.getUUID().toString());row.put("type",BuiltInRegistries.ENTITY_TYPE.getKey(entity.getType()).toString());row.put("position",List.of(entity.getX(),entity.getY(),entity.getZ()));row.put("visible",true);
            // World-space runtime geometry for this already-visible entity;
            // never infer a species size or expose an unseen entity here.
            var bounds=entity.getBoundingBox();row.put("bounds",List.of(bounds.minX,bounds.minY,bounds.minZ,bounds.maxX,bounds.maxY,bounds.maxZ));
            var velocity=entity.getDeltaMovement();row.put("velocity",List.of(velocity.x,velocity.y,velocity.z));
            row.put("hostile",entity instanceof Enemy||entity.getType().getCategory()==MobCategory.MONSTER);row.put("player",entity instanceof net.minecraft.world.entity.player.Player);
            if(entity instanceof LivingEntity living){row.put("health",living.getHealth());row.put("hurt_time",living.hurtTime);row.put("main_hand",equipment(living.getMainHandItem()));row.put("off_hand",equipment(living.getOffhandItem()));row.put("ranged_attack",entity instanceof RangedAttackMob||RANGED_ANIMATIONS.contains(living.getMainHandItem().getUseAnimation().name())||RANGED_ANIMATIONS.contains(living.getOffhandItem().getUseAnimation().name()));}
            if(entity instanceof LivingEntity living)row.put("item_use",visibleItemUse(living));
            if(entity instanceof Mob mob)row.put("aggressive",mob.isAggressive());
            if(entity instanceof Projectile projectile){var owner=projectile.getOwner();row.put("projectile",true);row.put("owner_uuid",owner==null?"":owner.getUUID().toString());}
            if(entity instanceof Creeper creeper)row.put("creeper",Map.of("swell_direction",creeper.getSwellDir(),"powered",creeper.isPowered(),"ignited",creeper.isIgnited()));
            if(entity instanceof ItemEntity item)row.put("item",stack(item.getItem(),player));
            entities.add(row);
        }
        var own=new LinkedHashMap<String,Object>();own.put("uuid",player.getUUID().toString());own.put("name",player.getGameProfile().getName());own.put("entity_id",player.getId());own.put("dimension",world.dimension().location().toString());own.put("position",List.of(player.getX(),player.getY(),player.getZ()));own.put("yaw",player.getYRot());own.put("pitch",player.getXRot());own.put("on_ground",player.onGround());own.put("sprinting",player.isSprinting());own.put("sneaking",player.isShiftKeyDown());own.put("health",player.getHealth());own.put("food",player.getFoodData().getFoodLevel());own.put("inventory",inventory);own.put("selected",player.getInventory().selected);
        own.put("life_id",life);own.put("native_action_effects",NativeEffects.snapshot(player));
        // Runtime eye/reach include the current pose and installed attributes.
        // These observations do not grant interaction authority or bypass LOS.
        var eye=player.getEyePosition();own.put("eye_position",List.of(eye.x,eye.y,eye.z));own.put("entity_interaction_range",player.entityInteractionRange());
        own.put("saturation",player.getFoodData().getSaturationLevel());own.put("air",player.getAirSupply());own.put("max_air",player.getMaxAirSupply());own.put("fire_ticks",player.getRemainingFireTicks());
        own.put("in_water",player.isInWater());own.put("eyes_in_water",player.isEyeInFluid(FluidTags.WATER));own.put("in_lava",player.isInLava());own.put("fall_distance",player.fallDistance);own.put("using_item",player.isUsingItem());
        own.put("use_item",stack(player.getUseItem(),player));own.put("remaining_use_ticks",player.getUseItemRemainingTicks());own.put("attack_strength",player.getAttackStrengthScale(0.5f));
        own.put("velocity",List.of(player.getDeltaMovement().x,player.getDeltaMovement().y,player.getDeltaMovement().z));
        own.put("movement_speed",player.getAttributeValue(Attributes.MOVEMENT_SPEED));own.put("attack_damage",player.getAttributeValue(Attributes.ATTACK_DAMAGE));
        own.put("attack_speed",player.getAttributeValue(Attributes.ATTACK_SPEED));own.put("max_health",player.getMaxHealth());own.put("armor",player.getArmorValue());
        var physics=new LinkedHashMap<String,Object>();physics.put("format","java767-physics-v1");
        physics.put("width",player.getBbWidth());physics.put("height",player.getBbHeight());physics.put("gravity",player.getAttributeValue(Attributes.GRAVITY));
        physics.put("jump_velocity",((dev.minecraftsdk.mixin.LivingPhysicsAccessor)player).sdk$jumpPower());physics.put("step_height",player.maxUpStep());
        physics.put("water_efficiency",player.getAttributeValue(Attributes.WATER_MOVEMENT_EFFICIENCY));physics.put("no_gravity",player.isNoGravity());
        physics.put("slow_falling",player.hasEffect(MobEffects.SLOW_FALLING));physics.put("dolphins_grace",player.hasEffect(MobEffects.DOLPHINS_GRACE));
        physics.put("levitation",player.hasEffect(MobEffects.LEVITATION)?player.getEffect(MobEffects.LEVITATION).getAmplifier():-1);
        physics.put("fall_flying",player.isFallFlying());physics.put("flying",player.getAbilities().flying);own.put("physics",physics);
        var mining=(dev.minecraftsdk.mixin.GameModeAccessor)player.gameMode;var target=mining.sdk$miningPosition();own.put("mining",Map.of("active",mining.sdk$mining(),"block",List.of(target.getX(),target.getY(),target.getZ())));own.put("accepted_chat",NativeChat.snapshot(player));
        var chat=NativeChat.inbox(player);
        return Map.of("revision",Integer.toString(server.getTickCount()),"player",own,"world",Map.of("game_time",world.getGameTime(),"day_time",world.getDayTime(),"blocks",blocks,"entities",entities,"unknown_columns",unknown,"chat",chat.get("messages"),"chat_epoch",chat.get("epoch"),"chat_dropped",chat.get("dropped"),"sample_clock",sampleClock,"auditory",NativeHearing.snapshot(player)),"menu",Map.of("menu_id",menu.containerId,"state_id",menu.getStateId(),"type",menu==player.inventoryMenu?"minecraft:inventory":BuiltInRegistries.MENU.getKey(menu.getType()).toString(),"slots",slots,"carried",stack(menu.getCarried(),player)));
    }
    private NativeObservation(){}
}
